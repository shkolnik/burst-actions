//! `burst up`: the full §3 lifecycle — lock/adopt, prepare, reconcile,
//! sweep-on-entry, fork-approval preflight, sizing, quota cap, image ensure,
//! then per-VM mint/launch/arm/record, then an observer-only watch.
//! Sizing is pure and offline-tested; the live quota probe lives on
//! `AwsContext` (`vcpu_headroom`, `vcpus_of`) and is exercised at the gate.

use crate::cloud::{Cloud, LaunchSpec};
use crate::config::Config;
use crate::error::Error;
use crate::github;
use crate::reconcile;
use crate::schema::{RepoId, TagSpec};
use crate::state::{InstanceRecord, RepoState, STATE_VERSION, StateFile};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub struct UpArgs {
    pub n: Option<u32>,
    pub auto: bool,
    pub spot: bool,
    pub yes: bool,
    pub ssh_key: Option<String>,
}

/// Poll until every watched instance is gone. Returns Detached if Ctrl-C
/// was received (closed set — the caller matches exhaustively).
#[derive(Debug, PartialEq, Eq)]
pub enum WatchOutcome {
    FleetGone,
    Detached { live: usize },
}

/// One VM at a time: each needs its own single-use JIT config, so
/// `RunInstances` is 1×N, never N×1. The statefile is written after every
/// single instance, so it trails reality by at most one — a SIGKILL between
/// any two calls leaves only tag-discoverable, kill-armed residue.
#[allow(clippy::too_many_arguments)]
pub fn launch_fleet(
    cloud: &mut impl Cloud,
    state: &RepoState,
    manifest: &mut StateFile,
    count: u32,
    expires: DateTime<Utc>,
    spot: bool,
    ssh_key: Option<&str>,
    image_id: &str,
    instance_type: &str,
    repo: &RepoId,
    mint: &mut dyn FnMut(&str) -> Result<String, Error>,
) -> Result<(), Error> {
    let mut launched = 0u32;
    let partial = |launched: u32, e: Error| Error::PartialLaunch {
        launched,
        requested: count,
        message: e.to_string(),
    };

    while launched < count {
        let nonce = github::runner_nonce();
        let name = github::runner_name(&nonce);
        let jit = mint(&name).map_err(|e| partial(launched, e))?;
        let user_data = crate::payload::fleet_user_data(&jit).map_err(|e| partial(launched, e))?;
        let instances = cloud
            .launch(&LaunchSpec {
                count: 1,
                image_id: image_id.to_string(),
                instance_type: instance_type.to_string(),
                spot,
                tags: TagSpec {
                    repo: repo.clone(),
                    expires,
                },
                user_data,
                ssh_key: ssh_key.map(str::to_string),
            })
            .map_err(|e| partial(launched, e))?;
        let Some(instance) = instances.into_iter().next() else {
            return Err(partial(
                launched,
                Error::Aws {
                    op: "RunInstances",
                    message: "returned no instance".to_string(),
                },
            ));
        };

        // An unfenced instance never survives an error path: if the kill
        // schedule cannot be armed, the instance it would have fenced dies
        // now (tag-verified by `terminate`) before we report the failure.
        if let Err(e) = cloud.arm_kill(&instance.id, expires) {
            let arm_err = partial(launched, e);
            match cloud.terminate(std::slice::from_ref(&instance.id)) {
                Ok(()) => return Err(arm_err),
                Err(term) => {
                    return Err(partial(
                        launched,
                        Error::Aws {
                            op: "TerminateInstances",
                            message: format!(
                                "could not arm the kill schedule for {id} and could not terminate it either ({term}) — terminate {id} by hand",
                                id = instance.id
                            ),
                        },
                    ));
                }
            }
        }

        manifest.instances.push(InstanceRecord {
            id: instance.id,
            launched_at: Utc::now(),
            expires_at: expires,
        });
        state.write(manifest).map_err(|e| partial(launched, e))?;
        launched += 1;
    }
    Ok(())
}

pub fn watch(
    cloud: &mut impl Cloud,
    repo: &RepoId,
    detach: &AtomicBool,
    poll: Duration,
) -> Result<WatchOutcome, Error> {
    let mut last: Option<usize> = None;
    loop {
        let live = cloud.list_tagged(repo)?.len();
        if last != Some(live) {
            println!("fleet: {live} live");
            last = Some(live);
        }
        if live == 0 {
            return Ok(WatchOutcome::FleetGone);
        }
        if detach.load(Ordering::SeqCst) {
            return Ok(WatchOutcome::Detached { live });
        }
        std::thread::sleep(poll);
    }
}

static DETACH: AtomicBool = AtomicBool::new(false);

pub fn run(config: &Config, args: &UpArgs) -> Result<(), Error> {
    // 1. Lock/adopt.
    let state = RepoState::open(&config.repo)?;
    let _lock = state.lock()?;
    let residue = state.read()?;

    // 2. Prepare — GitHub first, so a PAT problem aborts before any AWS
    //    resource exists.
    let mut p = super::image::prepare(config)?;

    // 3. Reconcile against cloud truth (tags are authoritative).
    let empty = StateFile {
        version: STATE_VERSION,
        repo: config.repo.to_string(),
        instances: Vec::new(),
    };
    let known = residue.unwrap_or(empty);
    let cloud_instances = p.cloud.list_tagged(&config.repo)?;
    let r = reconcile::reconcile(&known, &cloud_instances);
    for id in &r.dropped {
        println!("dropped {id} (no longer live)");
    }
    for id in &r.adopted {
        println!("adopted {id} (live and tagged, not in this host's statefile)");
    }
    if !r.adopted.is_empty() {
        let prompt = format!(
            "someone else appears to be running burst workers for {repo} from another host ({k} unrecognized live instances) — continue? [y/N] ",
            repo = config.repo,
            k = r.adopted.len()
        );
        let mut stdin = std::io::stdin().lock();
        if !super::down::confirm(&prompt, args.yes, &mut stdin) {
            println!("aborted");
            return Ok(());
        }
    }
    let mut manifest = StateFile {
        version: STATE_VERSION,
        repo: config.repo.to_string(),
        instances: r.live,
    };
    if manifest.instances.is_empty() {
        state.delete()?;
    } else {
        state.write(&manifest)?;
    }

    // 4. Sweep-on-entry: rent paid on entry.
    super::sweep::sweep_with(&mut p.cloud, &p.client, &config.repo)?;

    // 5. Preflight (invariant 5) — nothing has launched yet.
    let policy = p.client.fork_approval_policy(&config.repo)?;
    github::preflight_fork_approval(&config.repo, policy)?;

    // 6. Size.
    let auto_count = args
        .auto
        .then(|| p.client.queued_burst_job_count(&config.repo))
        .transpose()?;
    let requested = fleet_size(args.n, auto_count, config.max_fleet);
    if requested == 0 && manifest.instances.is_empty() {
        println!("no queued burst jobs — nothing to launch");
        return Ok(());
    }

    if requested > 0 {
        // 7. Quota.
        let vcpus = p.cloud.ctx.vcpus_of(&config.instance_type)?;
        let headroom = p.cloud.ctx.vcpu_headroom(args.spot)?;
        let (to_launch, warning) = quota_cap(requested, vcpus, headroom);
        if let Some(w) = warning {
            eprintln!("{w}");
        }

        // 8. AMI ensure — cache hit is the common ~zero-cost case.
        let image_id = p.cloud.bake(&p.key)?;

        // 9. Mint & launch & arm & record, one VM at a time.
        let expires = Utc::now() + ChronoDuration::hours(i64::from(config.ttl_hours));
        let client = &p.client;
        let repo = &config.repo;
        let mut mint = |name: &str| client.mint_jit_config(repo, name);
        launch_fleet(
            &mut p.cloud,
            &state,
            &mut manifest,
            to_launch,
            expires,
            args.spot,
            args.ssh_key.as_deref(),
            &image_id,
            &config.instance_type,
            repo,
            &mut mint,
        )?;
    }

    // 10. Watch — observer only (invariant 3).
    let _ = ctrlc::set_handler(|| DETACH.store(true, Ordering::SeqCst));
    match watch(&mut p.cloud, &config.repo, &DETACH, Duration::from_secs(30))? {
        WatchOutcome::Detached { live } => {
            println!(
                "detaching — fleet still running ({live} instances); it will finish and self-terminate. Re-run `burst up` to re-attach, `burst down` to tear down"
            );
            // The statefile stays: it is the adoptable-residue signal.
            Ok(())
        }
        WatchOutcome::FleetGone => {
            // 11. Final tidy: the one-shots are pointless now (idempotent if
            //     already fired).
            for rec in &manifest.instances {
                p.cloud.disarm_kill(&rec.id)?;
            }
            match p.client.list_runners(&config.repo).and_then(|runners| {
                for r in github::dead_registrations(&runners) {
                    p.client.delete_runner(&config.repo, r.id, &r.name)?;
                }
                Ok(())
            }) {
                Ok(()) => {}
                Err(e) => eprintln!(
                    "warning: GitHub registration tidy failed (instances are already down; next sweep retries): {e}"
                ),
            }
            state.delete()?;
            println!("fleet drained — all clean");
            Ok(())
        }
    }
}

/// Requested fleet size before quota: explicit N, or the --auto count,
/// capped at max_fleet. Zero is a valid answer (up prints "no queued burst
/// jobs — nothing to launch" and exits 0 — accurate, not degraded).
pub fn fleet_size(explicit_n: Option<u32>, auto_count: Option<u32>, max_fleet: u32) -> u32 {
    let n = explicit_n.or(auto_count).unwrap_or(0);
    n.min(max_fleet)
}

/// Decision 9: warn BEFORE capping, never half-launch silently. Returns the
/// launchable count and, when capped, the one warning message (single
/// authoring site).
pub fn quota_cap(
    requested: u32,
    vcpus_per_instance: u32,
    headroom_vcpus: u32,
) -> (u32, Option<String>) {
    let fits = headroom_vcpus
        .checked_div(vcpus_per_instance)
        .unwrap_or(requested);
    if fits >= requested {
        (requested, None)
    } else {
        (
            fits,
            Some(format!(
                "warning: vCPU quota caps the fleet — requested {requested} instances \
                 ({} vCPUs) but only {headroom_vcpus} vCPUs of quota headroom remain; \
                 launching {fits}. Leftover jobs fall to the home runner or a second \
                 `burst up` (request a quota increase in the AWS console to raise the cap)",
                requested * vcpus_per_instance
            )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::fake::FakeCloud;
    use crate::cloud::{Instance, InstanceState};

    fn repo() -> RepoId {
        RepoId::parse("octo/widgets").unwrap()
    }

    fn manifest() -> StateFile {
        StateFile {
            version: STATE_VERSION,
            repo: "octo/widgets".into(),
            instances: Vec::new(),
        }
    }

    /// Distinct, valid-base64-charset blobs so `fleet_user_data` accepts them
    /// and each VM's user-data is provably its own single-use config.
    fn blob(name: &str) -> String {
        name.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
    }

    #[test]
    fn launch_fleet_launches_n_arms_n_and_records_n() {
        let d = tempfile::tempdir().unwrap();
        let state = RepoState::open_at(d.path().to_path_buf());
        let mut m = manifest();
        let mut cloud = FakeCloud::default();
        let mut minted: Vec<String> = Vec::new();
        let mut mint = |name: &str| {
            minted.push(name.to_string());
            Ok(blob(name))
        };
        let expires = Utc::now() + ChronoDuration::hours(6);

        launch_fleet(
            &mut cloud,
            &state,
            &mut m,
            3,
            expires,
            false,
            None,
            "ami-x",
            "t3.micro",
            &repo(),
            &mut mint,
        )
        .unwrap();

        assert_eq!(cloud.list_tagged(&repo()).unwrap().len(), 3);
        assert_eq!(cloud.armed_kills().len(), 3);
        assert_eq!(m.instances.len(), 3);
        assert_eq!(state.read().unwrap().unwrap().instances.len(), 3);
        minted.sort();
        minted.dedup();
        assert_eq!(
            minted.len(),
            3,
            "each VM gets its own single-use JIT config"
        );
    }

    #[test]
    fn mint_failure_midway_is_partial_launch_and_the_launched_fleet_is_fenced() {
        let d = tempfile::tempdir().unwrap();
        let state = RepoState::open_at(d.path().to_path_buf());
        let mut m = manifest();
        let mut cloud = FakeCloud::default();
        let mut calls = 0u32;
        let mut mint = |name: &str| {
            calls += 1;
            if calls == 3 {
                Err(Error::GitHub {
                    op: "POST jitconfig",
                    status: 500,
                    message: "boom".into(),
                })
            } else {
                Ok(blob(name))
            }
        };

        let err = launch_fleet(
            &mut cloud,
            &state,
            &mut m,
            3,
            Utc::now() + ChronoDuration::hours(6),
            false,
            None,
            "ami-x",
            "t3.micro",
            &repo(),
            &mut mint,
        )
        .expect_err("mint failure must not be swallowed");

        let Error::PartialLaunch {
            launched,
            requested,
            message,
        } = &err
        else {
            panic!("expected PartialLaunch, got {err:?}")
        };
        assert_eq!((*launched, *requested), (2, 3));
        assert!(message.contains("boom"), "message must name the cause");
        assert_eq!(
            state.read().unwrap().unwrap().instances.len(),
            2,
            "the statefile must trail reality by at most one instance"
        );
        assert_eq!(cloud.armed_kills().len(), 2);
        assert_eq!(cloud.list_tagged(&repo()).unwrap().len(), 2);
    }

    /// A FakeCloud whose arm_kill always fails: the instance it would have
    /// fenced must not survive the error path.
    struct UnarmableCloud(FakeCloud);
    impl Cloud for UnarmableCloud {
        fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error> {
            self.0.launch(spec)
        }
        fn terminate(&mut self, ids: &[String]) -> Result<(), Error> {
            self.0.terminate(ids)
        }
        fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error> {
            self.0.list_tagged(repo)
        }
        fn arm_kill(&mut self, _id: &str, _at: DateTime<Utc>) -> Result<(), Error> {
            Err(Error::Aws {
                op: "CreateSchedule",
                message: "scheduler unavailable".into(),
            })
        }
        fn bake(&mut self, key: &str) -> Result<String, Error> {
            self.0.bake(key)
        }
        fn disarm_kill(&mut self, id: &str) -> Result<(), Error> {
            self.0.disarm_kill(id)
        }
        fn list_armed_kills(&self) -> Result<Vec<String>, Error> {
            self.0.list_armed_kills()
        }
        fn list_all_tagged(&self) -> Result<Vec<Instance>, Error> {
            self.0.list_all_tagged()
        }
    }

    #[test]
    fn arm_failure_terminates_the_unfenced_instance_and_errors_loud() {
        let d = tempfile::tempdir().unwrap();
        let state = RepoState::open_at(d.path().to_path_buf());
        let mut m = manifest();
        let mut cloud = UnarmableCloud(FakeCloud::default());
        let mut mint = |name: &str| Ok(blob(name));

        let err = launch_fleet(
            &mut cloud,
            &state,
            &mut m,
            2,
            Utc::now() + ChronoDuration::hours(6),
            false,
            None,
            "ami-x",
            "t3.micro",
            &repo(),
            &mut mint,
        )
        .expect_err("an unarmable instance must abort the fleet");

        let Error::PartialLaunch {
            launched,
            requested,
            message,
        } = &err
        else {
            panic!("expected PartialLaunch, got {err:?}")
        };
        assert_eq!((*launched, *requested), (0, 2));
        assert!(message.contains("scheduler unavailable"));
        assert!(
            cloud.list_tagged(&repo()).unwrap().is_empty(),
            "the unfenced instance must not survive the error path"
        );
        assert!(m.instances.is_empty());
    }

    /// Wrapper that reports the fleet live on the first poll and gone after,
    /// so `watch` must loop at least once before concluding.
    struct DrainingCloud {
        inner: FakeCloud,
        polls: std::cell::Cell<u32>,
    }
    impl Cloud for DrainingCloud {
        fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error> {
            self.inner.launch(spec)
        }
        fn terminate(&mut self, ids: &[String]) -> Result<(), Error> {
            self.inner.terminate(ids)
        }
        fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error> {
            self.polls.set(self.polls.get() + 1);
            if self.polls.get() > 1 {
                return Ok(Vec::new());
            }
            self.inner.list_tagged(repo)
        }
        fn arm_kill(&mut self, id: &str, at: DateTime<Utc>) -> Result<(), Error> {
            self.inner.arm_kill(id, at)
        }
        fn bake(&mut self, key: &str) -> Result<String, Error> {
            self.inner.bake(key)
        }
        fn disarm_kill(&mut self, id: &str) -> Result<(), Error> {
            self.inner.disarm_kill(id)
        }
        fn list_armed_kills(&self) -> Result<Vec<String>, Error> {
            self.inner.list_armed_kills()
        }
        fn list_all_tagged(&self) -> Result<Vec<Instance>, Error> {
            self.inner.list_all_tagged()
        }
    }

    fn plant(cloud: &mut FakeCloud, id: &str) {
        cloud.plant(Instance {
            id: id.to_string(),
            state: InstanceState::Running,
            tags: vec![
                ("burst-actions".to_string(), "1".to_string()),
                ("burst-actions-repo".to_string(), "octo/widgets".to_string()),
            ],
        });
    }

    #[test]
    fn watch_returns_fleet_gone_once_the_fleet_drains() {
        let mut inner = FakeCloud::default();
        plant(&mut inner, "i-aaa");
        let mut cloud = DrainingCloud {
            inner,
            polls: std::cell::Cell::new(0),
        };
        let detach = AtomicBool::new(false);
        assert_eq!(
            watch(&mut cloud, &repo(), &detach, Duration::ZERO).unwrap(),
            WatchOutcome::FleetGone
        );
        assert_eq!(cloud.polls.get(), 2, "must have polled again, not guessed");
    }

    #[test]
    fn watch_detaches_on_the_first_check_when_the_flag_is_already_set() {
        let mut cloud = FakeCloud::default();
        plant(&mut cloud, "i-aaa");
        plant(&mut cloud, "i-bbb");
        let detach = AtomicBool::new(true);
        assert_eq!(
            watch(&mut cloud, &repo(), &detach, Duration::ZERO).unwrap(),
            WatchOutcome::Detached { live: 2 }
        );
        assert_eq!(
            cloud.list_tagged(&repo()).unwrap().len(),
            2,
            "watch observes only — detaching must never terminate anything"
        );
    }

    #[test]
    fn fleet_size_explicit_wins_over_auto() {
        assert_eq!(fleet_size(Some(3), Some(10), 20), 3);
    }

    #[test]
    fn fleet_size_auto_capped_at_max_fleet() {
        assert_eq!(fleet_size(None, Some(50), 10), 10);
    }

    #[test]
    fn fleet_size_zero_flows_through() {
        assert_eq!(fleet_size(None, None, 10), 0);
        assert_eq!(fleet_size(Some(0), Some(5), 10), 0);
    }

    #[test]
    fn quota_cap_no_warning_when_it_fits_exactly() {
        let (n, warning) = quota_cap(4, 2, 8);
        assert_eq!(n, 4);
        assert!(warning.is_none());
    }

    #[test]
    fn quota_cap_capped_case_returns_smaller_count_and_message() {
        let (n, warning) = quota_cap(10, 4, 12);
        assert_eq!(n, 3);
        let msg = warning.expect("expected a warning when capped");
        assert!(msg.contains("warning"));
        assert!(msg.contains("10"));
        assert!(msg.contains("3"));
        assert!(msg.contains("quota increase"));
    }

    #[test]
    fn quota_cap_zero_vcpus_per_instance_never_divides_by_zero() {
        let (n, warning) = quota_cap(5, 0, 0);
        assert_eq!(n, 5);
        assert!(warning.is_none());
    }
}
