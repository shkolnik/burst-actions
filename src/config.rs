use crate::error::Error;
use crate::schema::{Arch, RepoId};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    burst: BurstTable,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BurstTable {
    repo: Option<String>,
    instance_type: Option<String>,
    region: Option<String>,
    max_fleet: Option<u32>,
    idle_timeout_min: Option<u32>,
    ttl_hours: Option<u32>,
    arch: Option<String>,
    base_ami: Option<String>,
    provision: Option<PathBuf>,
    budget_alarm_usd: Option<u32>,
}

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
        provision: table.provision,
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
