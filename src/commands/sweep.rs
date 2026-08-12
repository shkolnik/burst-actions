//! `burst sweep`: reap expired instances, orphan kill schedules, and dead
//! (never-connected) runner registrations. Repo-scoped for terminations —
//! like every command — but the orphan-schedule check must see every repo's
//! live instances, or it would mistake another repo's schedule for an
//! orphan.

use crate::cloud::{Cloud, Instance};
use crate::error::Error;
use crate::github;
use crate::schema::{RepoId, TAG_EXPIRES};
use chrono::{DateTime, Utc};

/// What a sweep decided to do — a closed set, so the report and the
/// executor stay exhaustive together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepAction {
    /// Instance past its burst-actions-expires (or with a missing/garbled
    /// expiry — mirroring reconcile: never trusted, treated as expired).
    TerminateExpired { instance_id: String },
    /// Armed schedule whose instance is no longer live anywhere.
    DisarmOrphanSchedule { instance_id: String },
    /// Never-connected burst-named offline registration.
    DeleteDeadRegistration { id: u64, name: String },
}

/// `TAG_EXPIRES` parsed RFC3339; parse failure or absence ⇒ expired —
/// mirrors `reconcile`'s adoption rule (never trust a missing/garbled tag).
fn is_expired(now: DateTime<Utc>, instance: &Instance) -> bool {
    instance
        .tags
        .iter()
        .find(|(k, _)| k == TAG_EXPIRES)
        .and_then(|(_, v)| DateTime::parse_from_rfc3339(v).ok())
        .map(|t| t.with_timezone(&Utc))
        .is_none_or(|expires_at| expires_at <= now)
}

/// Pure planner. `repo_instances` drives expiry (sweep is repo-scoped for
/// terminations, like every command); `all_live` spans ALL repos and exists
/// solely so another repo's live instance's schedule is never called an
/// orphan.
pub fn plan(
    now: DateTime<Utc>,
    repo_instances: &[Instance],
    all_live: &[Instance],
    armed: &[String],
    runners: &[github::RunnerRegistration],
    minted: &[String],
) -> Vec<SweepAction> {
    let mut actions = Vec::new();

    for instance in repo_instances {
        if is_expired(now, instance) {
            actions.push(SweepAction::TerminateExpired {
                instance_id: instance.id.clone(),
            });
        }
    }

    for id in armed {
        let live_elsewhere = all_live.iter().any(|i| &i.id == id && i.state.is_live());
        if !live_elsewhere {
            actions.push(SweepAction::DisarmOrphanSchedule {
                instance_id: id.clone(),
            });
        }
    }

    for r in github::dead_registrations(runners, minted) {
        actions.push(SweepAction::DeleteDeadRegistration {
            id: r.id,
            name: r.name.clone(),
        });
    }

    actions
}

/// Execute a plan. Terminates go through Cloud::terminate (which re-verifies
/// the burst-actions=1 tag immediately before acting); disarms through
/// Cloud::disarm_kill (idempotent); registration deletes through
/// Client::delete_runner (which re-verifies the name pattern). Idempotent by
/// construction: a second sweep plans nothing.
///
/// `DescribeInstances` (the source of `plan`'s `all_live`) is eventually
/// consistent: a concurrent `burst up` can have an instance running with its
/// kill schedule armed seconds before that instance is visible in listings.
/// `plan` alone could then call that live schedule an orphan. So immediately
/// before each disarm — not in `plan`, which stays pure over its inputs —
/// re-fetch `list_all_tagged` and re-check liveness; an instance that
/// appeared since planning is prove-absence-before-destroy's negative
/// result, and the disarm is skipped for the next sweep to re-plan.
pub fn execute(
    cloud: &mut impl Cloud,
    client: &github::Client,
    repo: &RepoId,
    actions: &[SweepAction],
) -> Result<(), Error> {
    for action in actions {
        match action {
            SweepAction::TerminateExpired { instance_id } => {
                cloud.terminate(std::slice::from_ref(instance_id))?;
            }
            SweepAction::DisarmOrphanSchedule { instance_id } => {
                let now_live = cloud
                    .list_all_tagged()?
                    .iter()
                    .any(|i| &i.id == instance_id && i.state.is_live());
                if now_live {
                    println!(
                        "sweep: skipping disarm of {instance_id} — it appeared live since planning; leaving for next sweep"
                    );
                } else {
                    cloud.disarm_kill(instance_id)?;
                }
            }
            SweepAction::DeleteDeadRegistration { id, name } => {
                client.delete_runner(repo, *id, name)?;
            }
        }
    }
    Ok(())
}

/// The single authoring site for sweep-action wording. `up`'s sweep-on-entry
/// prints through this too, so one event has exactly one spelling wherever
/// it is reported.
pub(crate) fn describe(action: &SweepAction) -> String {
    match action {
        SweepAction::TerminateExpired { instance_id } => {
            format!("terminate expired instance {instance_id}")
        }
        SweepAction::DisarmOrphanSchedule { instance_id } => {
            format!("disarm orphan kill schedule for {instance_id}")
        }
        SweepAction::DeleteDeadRegistration { id, name } => {
            format!("delete dead registration {name} (id {id})")
        }
    }
}

/// Best-effort tidy of dead GitHub registrations, shared by `up`'s final
/// tidy and `down`'s teardown tidy — the single authoring site for the
/// warning wording (identical instances-already-down/next-sweep-retries
/// message previously lived at both call sites). `client` is itself a
/// `Result` so a failure to even construct one (e.g. `down`'s
/// `token_from_env`) is reported through the same one-spelling path as an
/// API failure.
pub(crate) fn tidy_dead_registrations(
    client: Result<github::Client, Error>,
    repo: &RepoId,
    minted: &[String],
) {
    if minted.is_empty() {
        return;
    }
    let result = client.and_then(|client| {
        let runners = client.list_runners(repo)?;
        for r in github::dead_registrations(&runners, minted) {
            client.delete_runner(repo, r.id, &r.name)?;
        }
        Ok(())
    });
    if let Err(e) = result {
        eprintln!(
            "warning: GitHub registration tidy failed (instances are already down; next sweep retries): {e}"
        );
    }
}

/// Shared entry for up's sweep-on-entry: same list/plan/execute over an
/// already-prepared cloud+client, so up pays no second connect.
pub fn sweep_with(
    cloud: &mut crate::cloud::aws::AwsCloud,
    client: &github::Client,
    repo: &RepoId,
    minted: &[String],
) -> Result<Vec<SweepAction>, Error> {
    let repo_instances = cloud.list_tagged(repo)?;
    let all_live = cloud.list_all_tagged()?;
    let armed = cloud.list_armed_kills()?;
    // Registrations are manifest-scoped; skip the GitHub listing entirely
    // when this host minted nothing to look for.
    let runners = if minted.is_empty() {
        Vec::new()
    } else {
        client.list_runners(repo)?
    };

    let actions = plan(
        Utc::now(),
        &repo_instances,
        &all_live,
        &armed,
        &runners,
        minted,
    );
    execute(cloud, client, repo, &actions)?;
    Ok(actions)
}

/// `burst sweep`: prepare (Task 6), list, plan, execute, print one line per
/// action + a summary ("sweep: nothing to do" when empty). Takes the repo
/// lock up front, same as `up` — without it a same-host `up` and `sweep`
/// could race the eventually-consistent listing `execute`'s re-verify
/// depends on.
pub fn run(config: &crate::config::Config) -> Result<(), Error> {
    let state = crate::state::RepoState::open(&config.repo)?;
    run_locked(&state, config)
}

/// `run`, minus opening the repo's real (XDG-derived) statefile dir — takes
/// an already-opened `RepoState` so a held-lock fast-fail can be exercised
/// against a tempdir in tests, with no live AWS/GitHub reached.
fn run_locked(
    state: &crate::state::RepoState,
    config: &crate::config::Config,
) -> Result<(), Error> {
    let _lock = state.lock()?;
    // Registration tidy is manifest-scoped: only names this host's
    // statefile minted are ever candidates. No statefile ⇒ no names ⇒ the
    // GitHub side of the sweep has nothing it is entitled to touch.
    let minted: Vec<String> = state
        .read()?
        .map(|m| {
            m.instances
                .iter()
                .filter_map(|r| r.runner.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut p = super::image::prepare(config)?;
    let actions = sweep_with(&mut p.cloud, &p.client, &config.repo, &minted)?;
    if actions.is_empty() {
        println!("sweep: nothing to do");
    } else {
        for action in &actions {
            println!("sweep: {}", describe(action));
        }
        println!("sweep: {} action(s) taken", actions.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::fake::FakeCloud;
    use crate::cloud::{Instance, InstanceState};
    use crate::github::RunnerRegistration;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 9, 12, 0, 0).unwrap()
    }

    fn inst(id: &str, expires: Option<&str>) -> Instance {
        let mut tags = vec![
            ("burst-actions".to_string(), "1".to_string()),
            ("burst-actions-repo".to_string(), "octo/widgets".to_string()),
        ];
        if let Some(e) = expires {
            tags.push(("burst-actions-expires".to_string(), e.to_string()));
        }
        Instance {
            id: id.into(),
            state: InstanceState::Running,
            tags,
        }
    }

    fn runner(id: u64, name: &str, online: bool, busy: bool) -> RunnerRegistration {
        RunnerRegistration {
            id,
            name: name.to_string(),
            online,
            busy,
        }
    }

    #[test]
    fn expired_instance_selected_unexpired_not() {
        let expired = inst("i-old", Some("2026-08-09T11:00:00+00:00"));
        let live = inst("i-new", Some("2026-08-09T18:00:00+00:00"));
        let actions = plan(now(), &[expired.clone(), live], &[], &[], &[], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::TerminateExpired {
                instance_id: "i-old".into()
            }]
        );
    }

    #[test]
    fn missing_expiry_tag_is_selected() {
        let i = inst("i-notag", None);
        let actions = plan(now(), &[i], &[], &[], &[], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::TerminateExpired {
                instance_id: "i-notag".into()
            }]
        );
    }

    #[test]
    fn garbled_expiry_date_is_selected() {
        let i = inst("i-garbled", Some("not-a-date"));
        let actions = plan(now(), &[i], &[], &[], &[], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::TerminateExpired {
                instance_id: "i-garbled".into()
            }]
        );
    }

    #[test]
    fn armed_kill_for_another_repos_live_instance_is_not_an_orphan() {
        let other_repo_live = inst("i-other", Some("2026-08-09T18:00:00+00:00"));
        let actions = plan(
            now(),
            &[],
            &[other_repo_live],
            &["i-other".to_string()],
            &[],
            &[],
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn armed_kill_for_nowhere_live_instance_is_an_orphan() {
        let actions = plan(now(), &[], &[], &["i-gone".to_string()], &[], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::DisarmOrphanSchedule {
                instance_id: "i-gone".into()
            }]
        );
    }

    #[test]
    fn armed_kill_for_shutting_down_instance_is_an_orphan() {
        // list_all_tagged includes ShuttingDown instances, but
        // ShuttingDown.is_live() is false — a schedule for one must still be
        // planned as an orphan, matching reconcile::reconcile's precedent of
        // filtering on is_live() rather than mere id presence.
        let mut shutting_down = inst("i-going", Some("2026-08-09T18:00:00+00:00"));
        shutting_down.state = InstanceState::ShuttingDown;
        let actions = plan(
            now(),
            &[],
            &[shutting_down],
            &["i-going".to_string()],
            &[],
            &[],
        );
        assert_eq!(
            actions,
            vec![SweepAction::DisarmOrphanSchedule {
                instance_id: "i-going".into()
            }]
        );
    }

    #[test]
    fn dead_registrations_flow_through_unchanged() {
        let dead = runner(3, "burst-cccccccc", false, false);
        let online = runner(1, "burst-aaaaaaaa", true, false);
        let busy = runner(2, "burst-bbbbbbbb", false, true);
        let home = runner(4, "home", false, false);
        let minted = vec![
            "burst-cccccccc".to_string(),
            "burst-aaaaaaaa".to_string(),
            "burst-bbbbbbbb".to_string(),
        ];
        let actions = plan(
            now(),
            &[],
            &[],
            &[],
            &[dead.clone(), online, busy, home],
            &minted,
        );
        assert_eq!(
            actions,
            vec![SweepAction::DeleteDeadRegistration {
                id: dead.id,
                name: dead.name.clone(),
            }]
        );
    }

    #[test]
    fn empty_inputs_yield_empty_plan() {
        assert!(plan(now(), &[], &[], &[], &[], &[]).is_empty());
    }

    #[test]
    fn instance_appearing_between_plan_and_execute_is_not_disarmed() {
        // DescribeInstances is eventually consistent: plan can run when the
        // instance is still invisible even though its schedule is armed and
        // it's actually live. execute must re-verify absence immediately
        // before disarming, not trust plan's stale snapshot.
        let mut cloud = FakeCloud::default();
        cloud.arm_kill("i-racing", now()).unwrap();

        // Plan sees no live instance anywhere ⇒ orphan.
        let actions = plan(now(), &[], &[], &["i-racing".to_string()], &[], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::DisarmOrphanSchedule {
                instance_id: "i-racing".into()
            }]
        );

        // Between plan and execute, the concurrently-launched instance
        // becomes visible.
        cloud.plant(inst("i-racing", Some("2026-08-09T18:00:00+00:00")));

        let client = crate::github::Client::new(crate::github::Token::from("unused"));
        let repo = RepoId::parse("octo/widgets").unwrap();
        execute(&mut cloud, &client, &repo, &actions).unwrap();

        assert_eq!(
            cloud.armed_kills(),
            &[("i-racing".to_string(), now())],
            "schedule must survive — the instance is live, not orphaned"
        );
    }

    #[test]
    fn sweep_run_under_held_lock_fails_fast() {
        // A tempdir-backed RepoState, locked before run_locked is called —
        // mirrors `sweep::run`'s real lock/open ordering (via a shared
        // helper) without touching XDG/HOME or any live AWS/GitHub, since a
        // failed lock acquire must return before `image::prepare` runs.
        let d = tempfile::tempdir().unwrap();
        let state = crate::state::RepoState::open_at(d.path().to_path_buf());
        let _held = state.lock().unwrap();

        let config = crate::config::Config {
            repo: RepoId::parse("octo/widgets").unwrap(),
            instance_type: "t3.micro".into(),
            region: None,
            max_fleet: 1,
            idle_timeout_min: 10,
            ttl_hours: 1,
            arch: crate::schema::Arch::X86_64,
            base_ami: None,
            provision: None,
            volume: crate::schema::VolumeSpec::default(),
            budget_alarm_usd: None,
        };
        let result = run_locked(&state, &config);
        assert!(matches!(result, Err(Error::LockHeld { .. })), "{result:?}");
    }

    #[test]
    fn execute_terminates_and_disarms_then_replan_is_empty() {
        let mut cloud = FakeCloud::default();
        cloud.plant(inst("i-old", Some("2026-08-09T11:00:00+00:00")));
        cloud.arm_kill("i-orphan", now()).unwrap();

        let client = crate::github::Client::new(crate::github::Token::from("unused"));
        let repo = RepoId::parse("octo/widgets").unwrap();

        let repo_instances = cloud.list_tagged(&repo).unwrap();
        let all_live = cloud.list_all_tagged().unwrap();
        let armed = cloud.list_armed_kills().unwrap();
        let actions = plan(now(), &repo_instances, &all_live, &armed, &[], &[]);
        assert_eq!(actions.len(), 2, "{actions:?}");

        execute(&mut cloud, &client, &repo, &actions).unwrap();

        let listed = cloud.list_tagged(&repo).unwrap();
        assert!(listed.is_empty(), "{listed:?}");
        assert!(cloud.armed_kills().is_empty());

        // Idempotence: replanning over the post-execute state is empty.
        let repo_instances = cloud.list_tagged(&repo).unwrap();
        let all_live = cloud.list_all_tagged().unwrap();
        let armed = cloud.list_armed_kills().unwrap();
        let replanned = plan(now(), &repo_instances, &all_live, &armed, &[], &[]);
        assert!(replanned.is_empty(), "{replanned:?}");
    }
}
