//! VM payload assembly: the bake-time provisioning script and the per-VM
//! launch user-data.
//!
//! The provisioning script is built by embedding `vm/provision.sh.tmpl`
//! together with the wrapper and systemd unit files (all via `include_str!`
//! at compile time) and substituting placeholders. The rendered bytes are
//! fed straight into `schema::image_key` as the `provisioning_script`
//! input, so any change here changes the image key and forces a rebake.
//! Timeout values (`idle_timeout_min`, `ttl_hours`) deliberately do NOT
//! appear in the image: they travel in per-launch user-data as
//! `/etc/burst/launch.env`, so tuning them never forces a rebake.

use crate::error::Error;

const PROVISION_TMPL: &str = include_str!("../vm/provision.sh.tmpl");
const RUNNER_SH: &str = include_str!("../vm/burst-runner.sh");
const TTL_CHECK_SH: &str = include_str!("../vm/burst-ttl-check.sh");
const RUNNER_SERVICE: &str = include_str!("../vm/units/burst-runner.service");
const BOOTSTRAP_TIMER: &str = include_str!("../vm/units/burst-bootstrap-deadline.timer");
const BOOTSTRAP_SERVICE: &str = include_str!("../vm/units/burst-bootstrap-deadline.service");
const TTL_TIMER: &str = include_str!("../vm/units/burst-ttl.timer");
const TTL_SERVICE: &str = include_str!("../vm/units/burst-ttl.service");

/// Render the bake-time provisioning script.
///
/// Inlines the wrapper, check-script, and unit files into
/// `provision.sh.tmpl`'s heredoc markers, then substitutes the version
/// placeholder across the whole assembled script. Any `__BURST_...__`-shaped
/// placeholder still present after substitution is an authoring bug in a
/// template — reported by name rather than shipped into an AMI that would
/// silently ignore it.
pub fn render_provision(
    agent_version: &str,
    custom_provision: Option<&str>,
) -> Result<String, Error> {
    render_provision_from(PROVISION_TMPL, agent_version, custom_provision)
}

fn render_provision_from(
    tmpl: &str,
    agent_version: &str,
    custom_provision: Option<&str>,
) -> Result<String, Error> {
    let mut out = tmpl
        .replace("__BURST_RUNNER_SH__", RUNNER_SH)
        .replace("__BURST_TTL_CHECK_SH__", TTL_CHECK_SH)
        .replace("__BURST_RUNNER_SERVICE__", RUNNER_SERVICE)
        .replace("__BURST_BOOTSTRAP_TIMER__", BOOTSTRAP_TIMER)
        .replace("__BURST_BOOTSTRAP_SERVICE__", BOOTSTRAP_SERVICE)
        .replace("__BURST_TTL_TIMER__", TTL_TIMER)
        .replace("__BURST_TTL_SERVICE__", TTL_SERVICE);

    out = out.replace("__BURST_AGENT_VERSION__", agent_version);

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

    // The custom script is appended AFTER the placeholder check: it is the
    // user's own bytes, not a template — a literal `__BURST_...__` in it is
    // theirs to mean whatever they want. Appended bytes are part of the
    // image-key input, so editing the custom script forces a rebake.
    if let Some(custom) = custom_provision {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n# ---- custom provision (burst.toml `provision`) ----\n");
        out.push_str(custom);
    }

    Ok(out)
}

/// Wrap the rendered provisioning script for a bake-time builder run: write
/// it to disk and execute it. Success touches the bootstrap marker and
/// powers the builder off, so `CreateImage` finds a stopped instance;
/// failure leaves the instance running with no marker and no poweroff — the
/// builder has no SSM access, so the CLI's own bake timeout (not a remote
/// inspection) is what catches a stuck build.
pub fn wrap_provision_for_bake(provisioning_script: &str) -> Result<String, Error> {
    // The script is our own rendered template, but it is embedded in a
    // heredoc all the same: a line colliding with the terminator would
    // truncate it silently and execute the remainder as root shell.
    if provisioning_script
        .lines()
        .any(|l| l.trim() == "BURST_PROVISION_SCRIPT")
    {
        return Err(Error::Environment {
            reason: "provisioning script contains the heredoc terminator BURST_PROVISION_SCRIPT"
                .to_string(),
        });
    }
    Ok(format!(
        "#!/usr/bin/env bash\n\
         set -uo pipefail\n\
         install -d -m 0755 /var/lib/burst\n\
         install -m 0755 -o root -g root /dev/stdin /var/lib/burst/provision.sh <<'BURST_PROVISION_SCRIPT'\n\
         {provisioning_script}\n\
         BURST_PROVISION_SCRIPT\n\
         if /var/lib/burst/provision.sh; then\n\
         \x20   touch /var/lib/burst/provisioned && poweroff\n\
         fi\n"
    ))
}

/// Per-VM launch user-data: writes the launch env (idle/TTL timeouts) and
/// the single-use JIT config, then starts the runner unit. The
/// bootstrap-deadline and TTL timers are enabled at bake time and arm on
/// every boot independent of this user-data ever running — if it's absent
/// or broken, the bootstrap deadline still powers the instance off and the
/// TTL check falls back to its baked default.
pub fn fleet_user_data(
    jit_config: &str,
    idle_timeout_min: u32,
    ttl_hours: u32,
) -> Result<String, Error> {
    // The blob is embedded verbatim inside a root-executed heredoc. GitHub's
    // encoded JIT config is base64; anything outside that charset (in
    // particular a newline, which could smuggle a heredoc terminator plus
    // arbitrary shell) is rejected rather than embedded.
    if jit_config.is_empty()
        || !jit_config
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Err(Error::Environment {
            reason: "JIT config is not a base64 blob — refusing to embed it in launch user-data"
                .to_string(),
        });
    }
    // u32 formatting cannot produce shell metacharacters, so the env file
    // needs no escaping.
    Ok(format!(
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         install -d -m 0755 /etc/burst\n\
         printf 'IDLE_TIMEOUT_MIN=%s\\nTTL_HOURS=%s\\n' {idle_timeout_min} {ttl_hours} > /etc/burst/launch.env\n\
         install -m 0600 -o root -g root /dev/stdin /etc/burst/jitconfig <<'BURST_JITCONFIG'\n\
         {jit_config}\n\
         BURST_JITCONFIG\n\
         systemctl start burst-runner.service\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Arch, ImageKeyInputs, image_key};

    #[test]
    fn render_has_no_leftover_placeholders() {
        let rendered = render_provision("2.320.0", None).unwrap();
        assert!(!rendered.contains("__BURST_"), "{rendered}");
    }

    #[test]
    fn render_contains_expected_values() {
        let rendered = render_provision("2.320.0", None).unwrap();
        assert!(rendered.contains("--disableupdate"));
        assert!(rendered.contains("2.320.0"));
    }

    /// The custom script rides at the end of the rendered output — after the
    /// runner install and timer enablement — and its bytes change the image
    /// key, so editing it forces a rebake instead of reusing a stale AMI.
    #[test]
    fn custom_provision_appends_and_changes_the_image_key() {
        let base = render_provision("2.320.0", None).unwrap();
        let custom = render_provision("2.320.0", Some("apt-get install -y docker.io\n")).unwrap();
        assert!(
            custom.starts_with(&base),
            "custom must extend, not alter, the base"
        );
        assert!(
            custom.ends_with("apt-get install -y docker.io\n"),
            "{custom}"
        );
        let key = |script: &str| {
            image_key(&ImageKeyInputs {
                provisioning_script: script.as_bytes(),
                base_image_id: "ami-0abc",
                arch: Arch::X86_64,
                runner_agent_version: "2.320.0",
            })
        };
        assert_ne!(key(&base), key(&custom));
    }

    /// A `__BURST_...__` string inside the user's own script is their
    /// content, not an unsubstituted template placeholder.
    #[test]
    fn custom_provision_may_contain_placeholder_shaped_text() {
        render_provision("2.320.0", Some("echo __BURST_NOT_A_PLACEHOLDER__\n")).unwrap();
    }

    #[test]
    fn render_errors_on_leftover_placeholder() {
        let bad_tmpl = "echo hi\n__BURST_TYPO__\n";
        let err = render_provision_from(bad_tmpl, "2.320.0", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("__BURST_TYPO__"), "{msg}");
    }

    #[test]
    fn bake_wrapper_powers_off_only_on_success() {
        let wrapped = wrap_provision_for_bake("echo hi\n").unwrap();
        assert!(
            wrapped.contains("touch /var/lib/burst/provisioned && poweroff"),
            "{wrapped}"
        );
        // No bare `poweroff` on the failure path: every occurrence of
        // "poweroff" in the wrapper must be the success-only one above.
        assert_eq!(
            wrapped.matches("poweroff").count(),
            1,
            "expected exactly one poweroff, gated on success: {wrapped}"
        );
    }

    #[test]
    fn fleet_user_data_rejects_non_base64_blobs() {
        // A newline is the escape vector: it could smuggle the heredoc
        // terminator plus arbitrary root shell.
        for bad in ["evil\nBURST_JITCONFIG\npoweroff", "", "spaces in blob"] {
            let msg = fleet_user_data(bad, 10, 6).unwrap_err().to_string();
            assert!(msg.contains("not a base64 blob"), "{bad:?}: {msg}");
        }
    }

    #[test]
    fn bake_wrapper_rejects_terminator_collision() {
        let err = wrap_provision_for_bake("echo hi\nBURST_PROVISION_SCRIPT\n").unwrap_err();
        assert!(err.to_string().contains("BURST_PROVISION_SCRIPT"));
    }

    #[test]
    fn bake_wrapper_embeds_the_provisioning_script() {
        let wrapped = wrap_provision_for_bake("apt-get install -y foo\n").unwrap();
        assert!(wrapped.contains("apt-get install -y foo"));
    }

    #[test]
    fn fleet_user_data_contains_jitconfig_mode_and_start() {
        let ud = fleet_user_data("blob", 10, 6).unwrap();
        assert!(ud.contains("blob"));
        assert!(ud.contains("0600"));
        assert!(ud.contains("systemctl start burst-runner.service"));
    }

    /// Timeouts travel per-launch: the env file the on-VM readers source
    /// must carry exactly the configured values, written before the runner
    /// unit starts.
    #[test]
    fn fleet_user_data_writes_launch_env_before_runner_start() {
        let ud = fleet_user_data("blob", 7, 3).unwrap();
        let env_pos = ud.find("/etc/burst/launch.env").expect("env file written");
        assert!(ud.contains("IDLE_TIMEOUT_MIN=%s"), "{ud}");
        assert!(ud.contains(" 7 3 "), "values interpolated: {ud}");
        let start_pos = ud.find("systemctl start burst-runner.service").unwrap();
        assert!(
            env_pos < start_pos,
            "env must exist before the runner starts"
        );
    }

    /// The on-VM readers and the writer agree on one env file path and
    /// variable names — a rename on either side silently reverts a VM to
    /// the baked defaults.
    #[test]
    fn launch_env_contract_matches_on_vm_readers() {
        for reader in [RUNNER_SH, TTL_CHECK_SH] {
            assert!(reader.contains(". /etc/burst/launch.env"), "{reader}");
        }
        assert!(RUNNER_SH.contains("IDLE_TIMEOUT_MIN=10"), "baked default");
        assert!(TTL_CHECK_SH.contains("TTL_HOURS=6"), "baked default");
    }

    /// The bootstrap deadline (G3, layer 3) only works because the runner
    /// wrapper and the deadline check agree on one sentinel path. A rename
    /// in either file would silently disarm the layer: the deadline would
    /// check a path nobody touches and poweroff every healthy instance —
    /// or worse, never fire.
    #[test]
    fn bootstrap_deadline_checks_the_exact_path_the_runner_touches() {
        const SENTINEL: &str = "/run/burst/registered";
        assert!(
            RUNNER_SH.contains(&format!("touch {SENTINEL}")),
            "runner wrapper must touch the registration sentinel"
        );
        assert!(
            BOOTSTRAP_SERVICE.contains(&format!("[ -f {SENTINEL} ]")),
            "deadline must check the same sentinel path"
        );
    }

    /// Layers 3 (bootstrap deadline, TTL) are armed only because the bake
    /// enables their timers. Losing this line from the template ships an
    /// image whose on-VM cleanup never runs — exactly the G3/G4a properties
    /// verified live.
    #[test]
    fn provision_enables_both_cleanup_timers() {
        let rendered = render_provision("2.320.0", None).unwrap();
        assert!(
            rendered.contains("systemctl enable burst-bootstrap-deadline.timer burst-ttl.timer"),
            "bake must enable the bootstrap-deadline and ttl timers"
        );
    }

    /// The unit's ExecStart and the template's install path must name the
    /// same file, or the runner never starts and every instance dies at the
    /// bootstrap deadline. Same contract for the TTL check script.
    #[test]
    fn runner_unit_execstart_matches_the_installed_path() {
        assert!(
            RUNNER_SERVICE.contains("ExecStart=/opt/burst/burst-runner.sh"),
            "unit must exec the path provision installs"
        );
        assert!(
            PROVISION_TMPL.contains("/opt/burst/burst-runner.sh"),
            "provision must install the runner wrapper at the unit's path"
        );
        assert!(
            TTL_SERVICE.contains("ExecStart=/opt/burst/burst-ttl-check.sh"),
            "ttl unit must exec the path provision installs"
        );
        assert!(
            PROVISION_TMPL.contains("/opt/burst/burst-ttl-check.sh"),
            "provision must install the ttl check at the unit's path"
        );
    }

    /// The inverse of the old baked-timeout behavior: tuning timeouts must
    /// NOT change the rendered script (and therefore never forces a rebake)
    /// — the values travel in launch user-data instead.
    #[test]
    fn timeouts_do_not_appear_in_the_provisioning_script() {
        let rendered = render_provision("2.320.0", None).unwrap();
        let key = image_key(&ImageKeyInputs {
            provisioning_script: rendered.as_bytes(),
            base_image_id: "ami-0abc",
            arch: Arch::X86_64,
            runner_agent_version: "2.320.0",
        });
        // render_provision takes no timeout inputs at all — the type system
        // already proves independence; pin the runtime side too: the script
        // sources the launch env rather than embedding a value.
        assert!(rendered.contains("/etc/burst/launch.env"), "{key:?}");
    }
}
