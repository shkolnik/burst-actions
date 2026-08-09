//! `burst up`: fleet sizing (this task) plus launch orchestration (Task 11).
//! Sizing is pure and offline-tested; the live quota probe lives on
//! `AwsContext` (`vcpu_headroom`, `vcpus_of`) and is exercised at the gate.

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
