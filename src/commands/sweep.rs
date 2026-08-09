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
        let live_elsewhere = all_live.iter().any(|i| &i.id == id);
        if !live_elsewhere {
            actions.push(SweepAction::DisarmOrphanSchedule {
                instance_id: id.clone(),
            });
        }
    }

    for r in github::dead_registrations(runners) {
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
                cloud.disarm_kill(instance_id)?;
            }
            SweepAction::DeleteDeadRegistration { id, name } => {
                client.delete_runner(repo, *id, name)?;
            }
        }
    }
    Ok(())
}

fn describe(action: &SweepAction) -> String {
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

/// Shared entry for up's sweep-on-entry: same list/plan/execute over an
/// already-prepared cloud+client, so up pays no second connect.
pub fn sweep_with(
    cloud: &mut crate::cloud::aws::AwsCloud,
    client: &github::Client,
    repo: &RepoId,
) -> Result<Vec<SweepAction>, Error> {
    let repo_instances = cloud.list_tagged(repo)?;
    let all_live = cloud.list_all_tagged()?;
    let armed = cloud.list_armed_kills()?;
    let runners = client.list_runners(repo)?;

    let actions = plan(Utc::now(), &repo_instances, &all_live, &armed, &runners);
    execute(cloud, client, repo, &actions)?;
    Ok(actions)
}

/// `burst sweep`: prepare (Task 6), list, plan, execute, print one line per
/// action + a summary ("sweep: nothing to do" when empty).
pub fn run(config: &crate::config::Config) -> Result<(), Error> {
    let mut p = super::image::prepare(config)?;
    let actions = sweep_with(&mut p.cloud, &p.client, &config.repo)?;
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
        let actions = plan(now(), &[expired.clone(), live], &[], &[], &[]);
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
        let actions = plan(now(), &[i], &[], &[], &[]);
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
        let actions = plan(now(), &[i], &[], &[], &[]);
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
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn armed_kill_for_nowhere_live_instance_is_an_orphan() {
        let actions = plan(now(), &[], &[], &["i-gone".to_string()], &[]);
        assert_eq!(
            actions,
            vec![SweepAction::DisarmOrphanSchedule {
                instance_id: "i-gone".into()
            }]
        );
    }

    #[test]
    fn dead_registrations_flow_through_unchanged() {
        let dead = runner(3, "burst-cccccccc", false, false);
        let online = runner(1, "burst-aaaaaaaa", true, false);
        let busy = runner(2, "burst-bbbbbbbb", false, true);
        let home = runner(4, "home", false, false);
        let actions = plan(now(), &[], &[], &[], &[dead.clone(), online, busy, home]);
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
        assert!(plan(now(), &[], &[], &[], &[]).is_empty());
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
        let actions = plan(now(), &repo_instances, &all_live, &armed, &[]);
        assert_eq!(actions.len(), 2, "{actions:?}");

        execute(&mut cloud, &client, &repo, &actions).unwrap();

        let listed = cloud.list_tagged(&repo).unwrap();
        assert!(listed.is_empty(), "{listed:?}");
        assert!(cloud.armed_kills().is_empty());

        // Idempotence: replanning over the post-execute state is empty.
        let repo_instances = cloud.list_tagged(&repo).unwrap();
        let all_live = cloud.list_all_tagged().unwrap();
        let armed = cloud.list_armed_kills().unwrap();
        let replanned = plan(now(), &repo_instances, &all_live, &armed, &[]);
        assert!(replanned.is_empty(), "{replanned:?}");
    }
}
