//! VM payload assembly: the bake-time provisioning script and the per-VM
//! launch user-data.
//!
//! The provisioning script is built by embedding `vm/provision.sh.tmpl`
//! together with the wrapper and systemd unit files (all via `include_str!`
//! at compile time) and substituting placeholders. The rendered bytes are
//! fed straight into `schema::image_key` as the `provisioning_script`
//! input, so any change here — including a timeout value coming from
//! `burst.toml` — changes the image key and forces a rebake; the on-VM
//! timers can never silently drift from configuration.

use crate::error::Error;

const PROVISION_TMPL: &str = include_str!("../vm/provision.sh.tmpl");
const RUNNER_SH: &str = include_str!("../vm/burst-runner.sh");
const RUNNER_SERVICE: &str = include_str!("../vm/units/burst-runner.service");
const BOOTSTRAP_TIMER: &str = include_str!("../vm/units/burst-bootstrap-deadline.timer");
const BOOTSTRAP_SERVICE: &str = include_str!("../vm/units/burst-bootstrap-deadline.service");
const TTL_TIMER: &str = include_str!("../vm/units/burst-ttl.timer");
const TTL_SERVICE: &str = include_str!("../vm/units/burst-ttl.service");

/// Render the bake-time provisioning script.
///
/// Inlines the wrapper and unit files into `provision.sh.tmpl`'s heredoc
/// markers, then substitutes the timeout/version placeholders across the
/// whole assembled script (so placeholders inside the inlined files, e.g.
/// the idle timeout baked into `burst-runner.sh`, get substituted too).
/// Any `__BURST_...__`-shaped placeholder still present after substitution
/// is an authoring bug in a template — reported by name rather than shipped
/// into an AMI that would silently ignore it.
pub fn render_provision(
    idle_timeout_min: u32,
    ttl_hours: u32,
    agent_version: &str,
) -> Result<String, Error> {
    render_provision_from(PROVISION_TMPL, idle_timeout_min, ttl_hours, agent_version)
}

fn render_provision_from(
    tmpl: &str,
    idle_timeout_min: u32,
    ttl_hours: u32,
    agent_version: &str,
) -> Result<String, Error> {
    let mut out = tmpl
        .replace("__BURST_RUNNER_SH__", RUNNER_SH)
        .replace("__BURST_RUNNER_SERVICE__", RUNNER_SERVICE)
        .replace("__BURST_BOOTSTRAP_TIMER__", BOOTSTRAP_TIMER)
        .replace("__BURST_BOOTSTRAP_SERVICE__", BOOTSTRAP_SERVICE)
        .replace("__BURST_TTL_TIMER__", TTL_TIMER)
        .replace("__BURST_TTL_SERVICE__", TTL_SERVICE);

    out = out
        .replace("__BURST_IDLE_TIMEOUT_MIN__", &idle_timeout_min.to_string())
        .replace("__BURST_TTL_HOURS__", &ttl_hours.to_string())
        .replace("__BURST_AGENT_VERSION__", agent_version);

    if let Some(pos) = out.find("__BURST_") {
        let rest = &out[pos..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        return Err(Error::Environment {
            reason: format!(
                "provisioning template has unsubstituted placeholder {}",
                &rest[..end]
            ),
        });
    }

    Ok(out)
}

/// Per-VM launch user-data: writes the single-use JIT config and starts the
/// runner unit. The bootstrap-deadline and TTL timers are enabled at bake
/// time and arm on every boot independent of this user-data ever running —
/// if it's absent or broken, the bootstrap deadline still powers the
/// instance off.
pub fn fleet_user_data(jit_config: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         install -d -m 0755 /etc/burst\n\
         install -m 0600 -o root -g root /dev/stdin /etc/burst/jitconfig <<'BURST_JITCONFIG'\n\
         {jit_config}\n\
         BURST_JITCONFIG\n\
         systemctl start burst-runner.service\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Arch, ImageKeyInputs, image_key};

    #[test]
    fn render_has_no_leftover_placeholders() {
        let rendered = render_provision(10, 6, "2.320.0").unwrap();
        assert!(!rendered.contains("__BURST_"), "{rendered}");
    }

    #[test]
    fn render_contains_expected_values() {
        let rendered = render_provision(10, 6, "2.320.0").unwrap();
        assert!(rendered.contains("--disableupdate"));
        assert!(rendered.contains("10"));
        assert!(rendered.contains("2.320.0"));
    }

    #[test]
    fn render_errors_on_leftover_placeholder() {
        let bad_tmpl = "echo hi\n__BURST_TYPO__\n";
        let err = render_provision_from(bad_tmpl, 10, 6, "2.320.0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("__BURST_TYPO__"), "{msg}");
    }

    #[test]
    fn fleet_user_data_contains_jitconfig_mode_and_start() {
        let ud = fleet_user_data("blob");
        assert!(ud.contains("blob"));
        assert!(ud.contains("0600"));
        assert!(ud.contains("systemctl start burst-runner.service"));
    }

    #[test]
    fn different_ttl_hours_change_image_key() {
        let a = render_provision(10, 6, "2.320.0").unwrap();
        let b = render_provision(10, 12, "2.320.0").unwrap();
        assert_ne!(a, b);

        let key = |script: &str| {
            image_key(&ImageKeyInputs {
                provisioning_script: script.as_bytes(),
                base_image_id: "ami-0abc",
                arch: Arch::X86_64,
                runner_agent_version: "2.320.0",
            })
        };
        assert_ne!(key(&a), key(&b), "ttl_hours change must force a rebake");
    }
}
