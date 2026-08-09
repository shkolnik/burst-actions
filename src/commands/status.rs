//! `burst status`: cloud truth, read-only. No GitHub call, no bake-cache
//! lookup — just `list_tagged` + `list_armed_kills` against the substrate,
//! rendered as text.

use crate::cloud::aws::{AwsCloud, AwsContext};
use crate::cloud::{Cloud, Instance, InstanceState};
use crate::config::Config;
use crate::error::Error;
use crate::schema::{RepoId, TAG_EXPIRES};
use crate::state;
use chrono::{DateTime, Utc};

/// `InstanceState` as it reads in `status` output — one authoring site so
/// every state has exactly one spelling.
fn state_word(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Pending => "pending",
        InstanceState::Running => "running",
        InstanceState::ShuttingDown => "shutting-down",
        InstanceState::Terminated => "terminated",
        InstanceState::Stopping => "stopping",
        InstanceState::Stopped => "stopped",
    }
}

/// `TAG_EXPIRES` parsed RFC3339; missing/garbled ⇒ `None` (never trusted) —
/// mirrors sweep's `is_expired`, whose planner treats the same case as
/// expired.
fn expires_at(instance: &Instance) -> Option<DateTime<Utc>> {
    instance
        .tags
        .iter()
        .find(|(k, _)| k == TAG_EXPIRES)
        .and_then(|(_, v)| DateTime::parse_from_rfc3339(v).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// "in {h}h{m}m" — countdown to `expires` from `now`. Caller only invokes
/// this when `expires > now`.
fn countdown(now: DateTime<Utc>, expires: DateTime<Utc>) -> String {
    let total_minutes = (expires - now).num_minutes();
    format!("in {}h{}m", total_minutes / 60, total_minutes % 60)
}

fn instance_line(now: DateTime<Utc>, instance: &Instance, armed: &[String]) -> String {
    let kill = if armed.iter().any(|id| id == &instance.id) {
        "kill-armed"
    } else {
        "KILL SCHEDULE MISSING"
    };
    let expiry = match expires_at(instance) {
        Some(at) if at > now => {
            format!(
                "expires {} ({})",
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                countdown(now, at)
            )
        }
        Some(at) => format!(
            "expires {} EXPIRED",
            at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        None => "expires ??? EXPIRED".to_string(),
    };
    format!(
        "  {}  {}  {}  {}",
        instance.id,
        state_word(instance.state),
        expiry,
        kill
    )
}

/// Render the fleet as text. Pure — the one authoring site for status
/// wording (humans, scripts, and agents in CI logs all read it; one
/// spelling per condition).
pub fn render(
    repo: &RepoId,
    now: DateTime<Utc>,
    instances: &[Instance],
    armed: &[String],
    statefile_present: bool,
) -> String {
    let mut lines = Vec::new();
    if instances.is_empty() {
        lines.push(format!("fleet for {repo}: none"));
    } else {
        lines.push(format!("fleet for {repo}: {} live", instances.len()));
        for instance in instances {
            lines.push(instance_line(now, instance, armed));
        }
    }
    lines.push(if statefile_present {
        "statefile: present (a watcher was attached from this host)".to_string()
    } else {
        "statefile: none (no watcher from this host)".to_string()
    });
    lines.join("\n")
}

/// `burst status`: cloud truth only. Never calls `image::prepare` (which
/// hits GitHub and can fail on an unpinned AMI — wrong for a read-only
/// command); instead the minimal read path: connect, get-or-create the
/// substrate (pure gets on any account burst has touched before), an
/// `AwsCloud` with empty bake-only fields (never read by `list_*`), list,
/// render, print. Statefile presence is read without taking the lock —
/// status must work while a watcher is running.
pub fn run(config: &Config) -> Result<(), Error> {
    let ctx = AwsContext::connect(config.region.as_deref())?;
    let substrate = ctx.ensure_substrate(config.budget_alarm_usd)?;
    let cloud = AwsCloud {
        ctx,
        substrate,
        repo: config.repo.clone(),
        base_ami: String::new(),
        builder_instance_type: String::new(),
        provisioning_script: String::new(),
    };

    let instances = cloud.list_tagged(&config.repo)?;
    let armed = cloud.list_armed_kills()?;
    let statefile_present = state::RepoState::open(&config.repo)?.read()?.is_some();

    println!(
        "{}",
        render(
            &config.repo,
            Utc::now(),
            &instances,
            &armed,
            statefile_present
        )
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::InstanceState;
    use chrono::TimeZone;

    fn repo() -> RepoId {
        RepoId::parse("octo/widgets").unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 9, 12, 1, 30).unwrap()
    }

    fn inst(id: &str, state: InstanceState, expires: &str) -> Instance {
        Instance {
            id: id.into(),
            state,
            tags: vec![
                ("burst-actions".to_string(), "1".to_string()),
                ("burst-actions-repo".to_string(), "octo/widgets".to_string()),
                ("burst-actions-expires".to_string(), expires.to_string()),
            ],
        }
    }

    #[test]
    fn renders_running_and_pending_both_kill_armed() {
        let running = inst("i-0aaa", InstanceState::Running, "2026-08-09T18:00:00Z");
        let pending = inst("i-0bbb", InstanceState::Pending, "2026-08-09T18:00:00Z");
        let out = render(
            &repo(),
            now(),
            &[running, pending],
            &["i-0aaa".to_string(), "i-0bbb".to_string()],
            true,
        );
        assert_eq!(
            out,
            "fleet for octo/widgets: 2 live\n\
             \x20 i-0aaa  running  expires 2026-08-09T18:00:00Z (in 5h58m)  kill-armed\n\
             \x20 i-0bbb  pending  expires 2026-08-09T18:00:00Z (in 5h58m)  kill-armed\n\
             statefile: present (a watcher was attached from this host)"
        );
    }

    #[test]
    fn zero_fleet_is_none() {
        let out = render(&repo(), now(), &[], &[], false);
        assert_eq!(
            out,
            "fleet for octo/widgets: none\n\
             statefile: none (no watcher from this host)"
        );
    }

    #[test]
    fn missing_from_armed_shouts_kill_schedule_missing() {
        let running = inst("i-0aaa", InstanceState::Running, "2026-08-09T18:00:00Z");
        let out = render(&repo(), now(), &[running], &[], false);
        assert!(
            out.contains(
                "i-0aaa  running  expires 2026-08-09T18:00:00Z (in 5h58m)  KILL SCHEDULE MISSING"
            ),
            "{out}"
        );
    }

    #[test]
    fn past_expiry_prints_expired_in_place_of_countdown() {
        let expired = inst("i-0ccc", InstanceState::Running, "2026-08-09T11:00:00Z");
        let out = render(&repo(), now(), &[expired], &["i-0ccc".to_string()], false);
        assert!(
            out.contains("i-0ccc  running  expires 2026-08-09T11:00:00Z EXPIRED  kill-armed"),
            "{out}"
        );
    }

    #[test]
    fn exhaustive_state_word_covers_every_variant() {
        // Any variant added to InstanceState must add a state_word arm, or
        // this (and the real match) fails to compile — guard against the
        // "silently takes a wildcard" failure mode.
        for state in [
            InstanceState::Pending,
            InstanceState::Running,
            InstanceState::ShuttingDown,
            InstanceState::Terminated,
            InstanceState::Stopping,
            InstanceState::Stopped,
        ] {
            assert!(!state_word(state).is_empty());
        }
    }
}
