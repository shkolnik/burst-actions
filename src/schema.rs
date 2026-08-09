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
