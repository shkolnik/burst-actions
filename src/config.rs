use crate::error::Error;
use crate::schema::{Arch, DEFAULT_VOLUME_GB, RepoId, VolumeSpec};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    burst: BurstTable,
}

/// The `[burst]` table and its key list from one declaration: a key cannot be
/// added to the config without appearing in `KEYS`, which the example file is
/// checked against. Every key is optional in the file; `load` supplies the
/// defaults.
macro_rules! burst_table {
    ($($key:ident : $ty:ty),* $(,)?) => {
        #[derive(Debug, Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        struct BurstTable {
            $( $key: Option<$ty>, )*
        }

        /// Every settable `burst.toml` key, in declaration order.
        pub const KEYS: &[&str] = &[$(stringify!($key)),*];
    };
}

burst_table! {
    repo: String,
    instance_type: String,
    region: String,
    max_fleet: u32,
    idle_timeout_min: u32,
    ttl_hours: u32,
    arch: String,
    base_ami: String,
    provision: PathBuf,
    budget_alarm_usd: u32,
    volume_gb: u32,
    volume_iops: u32,
    volume_throughput_mbps: u32,
}

/// The annotated template shipped with the tool, written by `burst init` and
/// quoted verbatim in the README. Tests keep all three in step.
pub const EXAMPLE: &str = include_str!("../config.example.toml");

/// The line `burst init` rewrites with the caller's repo. A template edit that
/// loses it is a test failure, not a silently repo-less `burst.toml`.
pub const EXAMPLE_REPO_LINE: &str = "repo = \"owner/repo\"";

#[derive(Debug, Clone)]
pub struct Config {
    pub repo: RepoId,
    pub instance_type: String,
    pub region: Option<String>,
    pub max_fleet: u32,
    pub idle_timeout_min: u32,
    pub ttl_hours: u32,
    pub arch: Arch,
    pub base_ami: Option<String>,
    pub provision: Option<PathBuf>,
    /// Root volume every runner in the fleet launches with.
    pub volume: VolumeSpec,
    /// Monthly AWS Budgets cost alarm in USD (design §3 layer 5). Absent
    /// means no alarm is created — this is opt-in. Suggested value: $15.
    pub budget_alarm_usd: Option<u32>,
}

pub fn load(dir: &Path, repo_flag: Option<&str>) -> Result<Config, Error> {
    let path = dir.join("burst.toml");
    let table = if path.exists() {
        let text = std::fs::read_to_string(&path).map_err(|source| Error::ConfigRead {
            path: path.clone(),
            source,
        })?;
        toml::from_str::<FileConfig>(&text)
            .map_err(|e| Error::ConfigInvalid {
                path: path.clone(),
                reason: e.to_string(),
            })?
            .burst
    } else {
        BurstTable::default()
    };

    let invalid = |reason: String| Error::ConfigInvalid {
        path: path.clone(),
        reason,
    };

    let repo = match repo_flag.or(table.repo.as_deref()) {
        Some(r) => RepoId::parse(r)?,
        None => return Err(Error::RepoMissing),
    };
    let arch = match table.arch.as_deref() {
        None => Arch::default(),
        Some("x86_64") => Arch::X86_64,
        Some("arm64") => Arch::Arm64,
        Some(other) => return Err(invalid(format!("arch {other:?}: expected x86_64 or arm64"))),
    };
    let nonzero = |name: &str, v: Option<u32>, default: u32| match v {
        Some(0) => Err(invalid(format!("{name} must be at least 1"))),
        Some(n) => Ok(n),
        None => Ok(default),
    };
    let volume = VolumeSpec::new(
        table.volume_gb.unwrap_or(DEFAULT_VOLUME_GB),
        table.volume_iops,
        table.volume_throughput_mbps,
    )
    .map_err(invalid)?;
    let budget_alarm_usd = match table.budget_alarm_usd {
        Some(0) => return Err(invalid("budget_alarm_usd must be at least 1".into())),
        other => other,
    };

    Ok(Config {
        repo,
        instance_type: table.instance_type.unwrap_or_else(|| "c7i.2xlarge".into()),
        region: table.region,
        max_fleet: nonzero("max_fleet", table.max_fleet, 12)?,
        idle_timeout_min: nonzero("idle_timeout_min", table.idle_timeout_min, 10)?,
        ttl_hours: nonzero("ttl_hours", table.ttl_hours, 6)?,
        arch,
        base_ami: table.base_ami,
        // Resolved against burst.toml's directory, not the process cwd — the
        // config names a file in the repo it lives in.
        provision: table
            .provision
            .map(|p| if p.is_relative() { dir.join(p) } else { p }),
        volume,
        budget_alarm_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Arch;

    fn dir_with(toml: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("burst.toml"), toml).unwrap();
        d
    }

    #[test]
    fn defaults_apply_with_flag_repo_and_no_file() {
        let d = tempfile::tempdir().unwrap();
        let c = load(d.path(), Some("octo/widgets")).unwrap();
        assert_eq!(c.repo.to_string(), "octo/widgets");
        assert_eq!(c.instance_type, "c7i.2xlarge");
        assert_eq!(c.max_fleet, 12);
        assert_eq!(c.idle_timeout_min, 10);
        assert_eq!(c.ttl_hours, 6);
        assert_eq!(c.arch, Arch::X86_64);
        assert!(c.region.is_none() && c.base_ami.is_none() && c.provision.is_none());
        assert!(c.budget_alarm_usd.is_none());
    }

    /// The template's toggle convention: `#key = value` (no space after the
    /// `#`) is a commented-out setting, `# text` is prose. Uncommenting the
    /// former turns the template into the maximal config it documents.
    fn uncommented(example: &str) -> String {
        example
            .lines()
            .map(|l| match l.strip_prefix('#') {
                Some(rest) if !rest.starts_with(' ') && !rest.is_empty() => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drift guard, both directions. Uncommenting every documented setting
    /// must produce a config that `load` accepts — which catches a key that no
    /// longer exists (`deny_unknown_fields`), one whose type or allowed values
    /// changed, and one whose documented value is invalid. Then every key the
    /// code accepts must appear in the template.
    #[test]
    fn example_config_and_code_do_not_drift() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("burst.toml"), uncommented(EXAMPLE)).unwrap();
        let loaded = load(d.path(), None).expect("config.example.toml must load with all keys set");
        assert_eq!(loaded.repo.to_string(), "owner/repo");

        let table: toml::Table = toml::from_str(&uncommented(EXAMPLE)).unwrap();
        let documented = table["burst"].as_table().unwrap();
        for key in KEYS {
            assert!(
                documented.contains_key(*key),
                "config.example.toml documents no `{key}` — add it, or the key is undiscoverable"
            );
        }
    }

    /// `burst init` rewrites exactly this line; the README quotes the template
    /// verbatim. Both break silently if the template drifts from them.
    #[test]
    fn example_config_is_quoted_and_rewritable() {
        assert!(
            EXAMPLE.contains(&format!("\n{EXAMPLE_REPO_LINE}\n")),
            "template must carry the placeholder line `burst init` rewrites"
        );
        assert!(
            EXAMPLE.contains(crate::github::RUNS_ON),
            "the template must name the label a job needs to reach these runners"
        );
        assert!(
            include_str!("../README.md").contains(EXAMPLE.trim_end()),
            "README's configuration block has drifted from config.example.toml"
        );
    }

    #[test]
    fn volume_keys_load_and_default() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\n");
        assert_eq!(load(d.path(), None).unwrap().volume, VolumeSpec::default());
        let d = dir_with(
            "[burst]\nrepo = \"a/b\"\nvolume_gb = 750\nvolume_iops = 6000\nvolume_throughput_mbps = 1000\n",
        );
        let v = load(d.path(), None).unwrap().volume;
        assert_eq!(v, VolumeSpec::new(750, Some(6000), Some(1000)).unwrap());
    }

    /// A gp3 limit violation is a config error naming the key, not an EC2
    /// error hours later at RunInstances.
    #[test]
    fn volume_out_of_range_is_a_config_error() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\nvolume_gb = 99999\n");
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("volume_gb") && e.contains("burst.toml"), "{e}");
    }

    #[test]
    fn budget_alarm_usd_loads_when_set() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\nbudget_alarm_usd = 15\n");
        let c = load(d.path(), None).unwrap();
        assert_eq!(c.budget_alarm_usd, Some(15));
    }

    #[test]
    fn budget_alarm_usd_zero_rejected() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\nbudget_alarm_usd = 0\n");
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("budget_alarm_usd"), "{e}");
    }

    #[test]
    fn file_values_load_and_flag_overrides_repo() {
        let d =
            dir_with("[burst]\nrepo = \"a/b\"\ninstance_type = \"c7i.4xlarge\"\nmax_fleet = 3\n");
        let c = load(d.path(), None).unwrap();
        assert_eq!(c.repo.to_string(), "a/b");
        assert_eq!(c.instance_type, "c7i.4xlarge");
        assert_eq!(c.max_fleet, 3);
        let c2 = load(d.path(), Some("octo/widgets")).unwrap();
        assert_eq!(c2.repo.to_string(), "octo/widgets");
    }

    /// `provision` names a file in the repo burst.toml lives in — a relative
    /// path resolves against that directory, never the process cwd.
    #[test]
    fn provision_relative_path_resolves_against_config_dir() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\nprovision = \".burst/provision.sh\"\n");
        let c = load(d.path(), None).unwrap();
        assert_eq!(c.provision.unwrap(), d.path().join(".burst/provision.sh"));
        let d2 = dir_with("[burst]\nrepo = \"a/b\"\nprovision = \"/abs/provision.sh\"\n");
        let c2 = load(d2.path(), None).unwrap();
        assert_eq!(c2.provision.unwrap(), PathBuf::from("/abs/provision.sh"));
    }

    #[test]
    fn missing_repo_names_both_remedies() {
        let d = tempfile::tempdir().unwrap();
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("--repo") && e.contains("burst.toml"), "{e}");
    }

    #[test]
    fn unknown_key_is_a_hard_error() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\ninstance_typo = \"x\"\n");
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("instance_typo"), "{e}");
    }

    #[test]
    fn zero_limits_rejected() {
        for bad in ["max_fleet = 0", "idle_timeout_min = 0", "ttl_hours = 0"] {
            let d = dir_with(&format!("[burst]\nrepo = \"a/b\"\n{bad}\n"));
            assert!(load(d.path(), None).is_err(), "accepted {bad}");
        }
    }
}
