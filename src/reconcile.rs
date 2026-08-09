use crate::cloud::Instance;
use crate::schema::TAG_EXPIRES;
use crate::state::{InstanceRecord, StateFile};
use chrono::{DateTime, Utc};

pub struct Reconciled {
    /// The full post-reconciliation manifest: statefile records still alive in
    /// the cloud, plus adopted ones.
    pub live: Vec<InstanceRecord>,
    /// Ids present in the cloud but absent from the statefile (tags are
    /// authoritative — adopted into `live`).
    pub adopted: Vec<String>,
    /// Ids present in the statefile but no longer alive — dropped.
    pub dropped: Vec<String>,
}

pub fn reconcile(state: &StateFile, cloud: &[Instance]) -> Reconciled {
    let live_cloud: Vec<&Instance> = cloud.iter().filter(|i| i.state.is_live()).collect();
    let mut live = Vec::new();
    let mut dropped = Vec::new();
    let mut adopted = Vec::new();

    for rec in &state.instances {
        if live_cloud.iter().any(|i| i.id == rec.id) {
            live.push(rec.clone());
        } else {
            dropped.push(rec.id.clone());
        }
    }
    for inst in live_cloud {
        if state.instances.iter().any(|r| r.id == inst.id) {
            continue;
        }
        adopted.push(inst.id.clone());
        let expires_at = inst
            .tags
            .iter()
            .find(|(k, _)| k == TAG_EXPIRES)
            .and_then(|(_, v)| DateTime::parse_from_rfc3339(v).ok())
            .map(|t| t.with_timezone(&Utc))
            // Missing/garbled expiry: treat as already expired so the sweep
            // reaps it, never as trusted.
            .unwrap_or(DateTime::UNIX_EPOCH);
        live.push(InstanceRecord {
            id: inst.id.clone(),
            // Adopted: its JIT config (and so its runner name) was minted
            // elsewhere — this manifest can watch it die but never tidy its
            // registration.
            runner: None,
            launched_at: DateTime::UNIX_EPOCH,
            expires_at,
        });
    }
    Reconciled {
        live,
        adopted,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{Instance, InstanceState};
    use crate::state::{InstanceRecord, StateFile};
    use chrono::{TimeZone, Utc};

    fn rec(id: &str) -> InstanceRecord {
        let t = Utc.with_ymd_and_hms(2026, 8, 8, 12, 0, 0).unwrap();
        InstanceRecord {
            id: id.into(),
            runner: Some(format!("burst-{id}")),
            launched_at: t,
            expires_at: t,
        }
    }

    fn inst(id: &str, state: InstanceState, expires: Option<&str>) -> Instance {
        let mut tags = vec![
            ("burst-actions".to_string(), "1".to_string()),
            ("burst-actions-repo".to_string(), "octo/widgets".to_string()),
        ];
        if let Some(e) = expires {
            tags.push(("burst-actions-expires".to_string(), e.to_string()));
        }
        Instance {
            id: id.into(),
            state,
            tags,
        }
    }

    fn state_of(ids: &[&str]) -> StateFile {
        StateFile {
            version: 1,
            repo: "octo/widgets".into(),
            instances: ids.iter().map(|i| rec(i)).collect(),
        }
    }

    #[test]
    fn gone_instances_drop_unknown_live_ones_adopt() {
        let state = state_of(&["i-a", "i-b"]);
        let cloud = vec![
            inst(
                "i-a",
                InstanceState::Running,
                Some("2026-08-08T18:00:00+00:00"),
            ),
            inst(
                "i-c",
                InstanceState::Running,
                Some("2026-08-08T18:00:00+00:00"),
            ),
        ];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.dropped, vec!["i-b"]);
        assert_eq!(r.adopted, vec!["i-c"]);
        let mut live: Vec<&str> = r.live.iter().map(|i| i.id.as_str()).collect();
        live.sort();
        assert_eq!(live, vec!["i-a", "i-c"]);
        let c = r.live.iter().find(|i| i.id == "i-c").unwrap();
        assert_eq!(
            c.expires_at,
            Utc.with_ymd_and_hms(2026, 8, 8, 18, 0, 0).unwrap()
        );
    }

    #[test]
    fn shutting_down_counts_as_gone() {
        let state = state_of(&["i-a"]);
        let cloud = vec![inst("i-a", InstanceState::ShuttingDown, None)];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.dropped, vec!["i-a"]);
        assert!(r.live.is_empty() && r.adopted.is_empty());
    }

    #[test]
    fn adopted_instance_with_bad_expires_tag_is_treated_as_expired() {
        let state = state_of(&[]);
        let cloud = vec![inst("i-x", InstanceState::Running, Some("not-a-date"))];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.adopted, vec!["i-x"]);
        assert_eq!(r.live[0].expires_at, chrono::DateTime::UNIX_EPOCH);
    }

    #[test]
    fn empty_everything_reconciles_to_empty() {
        let r = reconcile(&state_of(&[]), &[]);
        assert!(r.live.is_empty() && r.adopted.is_empty() && r.dropped.is_empty());
    }
}
