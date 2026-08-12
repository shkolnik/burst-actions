use crate::error::Error;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::fmt;

pub const TAG_BURST: &str = "burst-actions";
pub const TAG_REPO: &str = "burst-actions-repo";
pub const TAG_EXPIRES: &str = "burst-actions-expires";
pub const TAG_IMAGE_KEY: &str = "burst-actions-image-key";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    owner: String,
    name: String,
}

impl RepoId {
    pub fn parse(s: &str) -> Result<Self, Error> {
        let err = || Error::RepoInvalid {
            given: s.to_string(),
        };
        let (owner, name) = s.split_once('/').ok_or_else(err)?;
        let ok = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        if !ok(owner) || !ok(name) {
            return Err(err());
        }
        Ok(RepoId {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn slug(&self) -> String {
        format!("{}-{}", self.owner, self.name)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone)]
pub struct TagSpec {
    pub repo: RepoId,
    pub expires: DateTime<Utc>,
}

impl TagSpec {
    pub fn to_tags(&self) -> [(String, String); 3] {
        [
            (TAG_BURST.into(), "1".into()),
            (TAG_REPO.into(), self.repo.to_string()),
            (
                TAG_EXPIRES.into(),
                self.expires.to_rfc3339_opts(SecondsFormat::Secs, true),
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arch {
    #[default]
    X86_64,
    Arm64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }
}

/// The root EBS volume a burst VM launches with: gp3 always, sized by the
/// consuming repo. `iops`/`throughput_mbps` left `None` take gp3's baseline
/// (3000 IOPS, 125 MB/s). The volume is the *only* disk — the job workspace
/// and the container data root both live on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeSpec {
    pub size_gb: u32,
    pub iops: Option<u32>,
    pub throughput_mbps: Option<u32>,
}

/// §8.2 starting default: big enough for a checkout, a toolchain and a
/// couple of container images; a disk-heavy repo raises `volume_gb`.
pub const DEFAULT_VOLUME_GB: u32 = 100;

impl Default for VolumeSpec {
    fn default() -> Self {
        Self {
            size_gb: DEFAULT_VOLUME_GB,
            iops: None,
            throughput_mbps: None,
        }
    }
}

impl VolumeSpec {
    /// gp3's published limits: 1-16384 GiB, 3000-16000 IOPS capped at 500
    /// IOPS/GiB, 125-1000 MB/s capped at 0.25 MB/s per provisioned IOPS.
    /// Checked here so a bad number fails at config load, not as an EC2 API
    /// error after bake, preflight and JIT minting.
    pub fn new(
        size_gb: u32,
        iops: Option<u32>,
        throughput_mbps: Option<u32>,
    ) -> Result<Self, String> {
        if !(1..=16384).contains(&size_gb) {
            return Err(format!("volume_gb {size_gb}: gp3 allows 1-16384"));
        }
        if let Some(i) = iops {
            if !(3000..=16000).contains(&i) {
                return Err(format!("volume_iops {i}: gp3 allows 3000-16000"));
            }
            if i > size_gb.saturating_mul(500) {
                return Err(format!(
                    "volume_iops {i}: gp3 allows at most 500 IOPS per GiB, so {size_gb} GiB caps at {}",
                    size_gb.saturating_mul(500).min(16000)
                ));
            }
        }
        if let Some(t) = throughput_mbps {
            if !(125..=1000).contains(&t) {
                return Err(format!("volume_throughput_mbps {t}: gp3 allows 125-1000"));
            }
            let provisioned_iops = iops.unwrap_or(3000);
            if t * 4 > provisioned_iops {
                return Err(format!(
                    "volume_throughput_mbps {t}: gp3 allows at most 0.25 MB/s per IOPS, so {provisioned_iops} IOPS caps at {} — raise volume_iops",
                    provisioned_iops / 4
                ));
            }
        }
        Ok(Self {
            size_gb,
            iops,
            throughput_mbps,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageKeyInputs<'a> {
    pub provisioning_script: &'a [u8],
    pub base_image_id: &'a str,
    pub arch: Arch,
    pub runner_agent_version: &'a str,
}

/// Content-addressed image cache key: v1- + 8 bytes of SHA-256 over the
/// length-prefixed inputs (length prefix so field boundaries can't alias).
pub fn image_key(i: &ImageKeyInputs) -> String {
    let mut h = Sha256::new();
    for field in [
        i.provisioning_script,
        i.base_image_id.as_bytes(),
        i.arch.as_str().as_bytes(),
        i.runner_agent_version.as_bytes(),
    ] {
        h.update((field.len() as u64).to_be_bytes());
        h.update(field);
    }
    format!("v1-{}", hex::encode(&h.finalize()[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn repo_id_parses_and_slugs() {
        let r = RepoId::parse("octo/widgets").unwrap();
        assert_eq!(r.owner(), "octo");
        assert_eq!(r.name(), "widgets");
        assert_eq!(r.to_string(), "octo/widgets");
        assert_eq!(r.slug(), "octo-widgets");
    }

    #[test]
    fn repo_id_rejects_malformed() {
        for bad in ["", "noslash", "a//b", "/x", "x/", "a/b/c", "we ird/repo"] {
            assert!(RepoId::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn tag_spec_emits_exact_schema() {
        let expires = chrono::Utc.with_ymd_and_hms(2026, 8, 8, 18, 0, 0).unwrap();
        let t = TagSpec {
            repo: RepoId::parse("octo/widgets").unwrap(),
            expires,
        };
        assert_eq!(
            t.to_tags(),
            [
                ("burst-actions".into(), "1".into()),
                ("burst-actions-repo".into(), "octo/widgets".into()),
                (
                    "burst-actions-expires".into(),
                    "2026-08-08T18:00:00Z".into()
                ),
            ]
        );
    }

    #[test]
    fn volume_spec_accepts_the_shapes_a_consumer_asks_for() {
        assert_eq!(VolumeSpec::default().size_gb, 100);
        let v = VolumeSpec::new(750, Some(6000), Some(1000)).unwrap();
        assert_eq!(
            (v.size_gb, v.iops, v.throughput_mbps),
            (750, Some(6000), Some(1000))
        );
        // Baseline gp3: both performance knobs absent is the common case.
        assert!(VolumeSpec::new(750, None, None).is_ok());
        // 750 MB/s is the ceiling at gp3's baseline 3000 IOPS.
        assert!(VolumeSpec::new(750, None, Some(750)).is_ok());
    }

    /// Every rejection names the offending key and its allowed range: these
    /// land at config load, hours before the EC2 call they would otherwise
    /// fail at.
    #[test]
    fn volume_spec_rejects_out_of_range_and_says_why() {
        for (size, iops, tput, want) in [
            (0, None, None, "volume_gb"),
            (20000, None, None, "volume_gb"),
            (750, Some(2999), None, "volume_iops"),
            (750, Some(16001), None, "volume_iops"),
            // 500 IOPS/GiB ceiling: 4 GiB caps at 2000, below the 3000 floor.
            (10, Some(5001), None, "500 IOPS per GiB"),
            (750, None, Some(124), "volume_throughput_mbps"),
            (750, None, Some(1001), "volume_throughput_mbps"),
            // 0.25 MB/s per IOPS: 1000 MB/s needs 4000 provisioned IOPS.
            (750, None, Some(1000), "raise volume_iops"),
        ] {
            let e = VolumeSpec::new(size, iops, tput).unwrap_err();
            assert!(e.contains(want), "{size}/{iops:?}/{tput:?} said {e:?}");
        }
    }

    #[test]
    fn image_key_stable_and_input_sensitive() {
        let base = ImageKeyInputs {
            provisioning_script: b"#!/bin/sh\napt-get install -y foo\n",
            base_image_id: "ami-0abc",
            arch: Arch::X86_64,
            runner_agent_version: "2.320.0",
        };
        let k = image_key(&base);
        assert_eq!(k, image_key(&base), "key must be deterministic");
        assert!(k.starts_with("v1-") && k.len() == 3 + 16, "{k}");
        for changed in [
            ImageKeyInputs {
                provisioning_script: b"#!/bin/sh\n",
                ..base
            },
            ImageKeyInputs {
                base_image_id: "ami-0abd",
                ..base
            },
            ImageKeyInputs {
                arch: Arch::Arm64,
                ..base
            },
            ImageKeyInputs {
                runner_agent_version: "2.321.0",
                ..base
            },
        ] {
            assert_ne!(k, image_key(&changed));
        }
    }

    #[test]
    fn image_key_fields_are_delimited() {
        // "ab" + "c" must not hash equal to "a" + "bc"
        let a = ImageKeyInputs {
            provisioning_script: b"ab",
            base_image_id: "c",
            arch: Arch::X86_64,
            runner_agent_version: "v",
        };
        let b = ImageKeyInputs {
            provisioning_script: b"a",
            base_image_id: "bc",
            ..a
        };
        assert_ne!(image_key(&a), image_key(&b));
    }
}
