use crate::error::Error;
use crate::schema::RepoId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub id: String,
    pub launched_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub version: u32,
    pub repo: String,
    pub instances: Vec<InstanceRecord>,
}

/// Minimal probe for the version field only, so a version mismatch is detected
/// (and reported with the version message) before attempting a full-shape
/// deserialization that would otherwise fail with a confusing "missing field"
/// error on any future incompatible version.
#[derive(Debug, Deserialize)]
struct VersionProbe {
    version: u32,
}

pub struct RepoState {
    dir: PathBuf,
    expected_repo: Option<String>,
}

pub(crate) fn state_root_from(
    xdg: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, Error> {
    if let Some(x) = xdg {
        return Ok(PathBuf::from(x).join("burst"));
    }
    if let Some(h) = home {
        return Ok(PathBuf::from(h).join(".local/state/burst"));
    }
    Err(Error::Environment {
        reason: "neither XDG_STATE_HOME nor HOME is set; cannot locate the burst state dir".into(),
    })
}

impl RepoState {
    pub fn open(repo: &RepoId) -> Result<Self, Error> {
        let root = state_root_from(
            std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()),
            std::env::var_os("HOME").filter(|v| !v.is_empty()),
        )?;
        let dir = root.join(repo.slug());
        std::fs::create_dir_all(&dir).map_err(|source| Error::State {
            path: dir.clone(),
            source,
        })?;
        Ok(RepoState {
            dir,
            expected_repo: Some(repo.to_string()),
        })
    }

    pub fn open_at(dir: PathBuf) -> Self {
        RepoState {
            dir,
            expected_repo: None,
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn read(&self) -> Result<Option<StateFile>, Error> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::State { path, source }),
        };
        let probe: VersionProbe = serde_json::from_str(&text).map_err(|e| Error::StateCorrupt {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if probe.version != STATE_VERSION {
            return Err(Error::StateCorrupt {
                path,
                reason: format!("unknown statefile version {}", probe.version),
            });
        }
        let state: StateFile = serde_json::from_str(&text).map_err(|e| Error::StateCorrupt {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if let Some(expected) = &self.expected_repo
            && &state.repo != expected
        {
            return Err(Error::StateCorrupt {
                path,
                reason: format!(
                    "statefile repo {:?} does not match expected repo {expected:?}",
                    state.repo
                ),
            });
        }
        Ok(Some(state))
    }

    pub fn write(&self, state: &StateFile) -> Result<(), Error> {
        if state.version != STATE_VERSION {
            return Err(Error::StateCorrupt {
                path: self.path(),
                reason: format!(
                    "refusing to write statefile version {} — this build only reads version {}",
                    state.version, STATE_VERSION
                ),
            });
        }
        let tmp = self.dir.join("state.json.tmp");
        let err = |source| Error::State {
            path: tmp.clone(),
            source,
        };
        let mut f = std::fs::File::create(&tmp).map_err(err)?;
        f.write_all(
            serde_json::to_string_pretty(state)
                .expect("statefile serializes")
                .as_bytes(),
        )
        .map_err(err)?;
        f.sync_all().map_err(err)?;
        std::fs::rename(&tmp, self.path()).map_err(|source| Error::State {
            path: self.path(),
            source,
        })?;
        // fsync the directory too: on POSIX, fsyncing the renamed file does not
        // guarantee the rename's directory-entry update is durable — a crash
        // right after rename() can lose the new name (or leave both names) even
        // though the file's own data was synced. Only fsyncing the containing
        // directory makes the rename itself durable.
        let dir = std::fs::File::open(&self.dir).map_err(|source| Error::State {
            path: self.dir.clone(),
            source,
        })?;
        dir.sync_all().map_err(|source| Error::State {
            path: self.dir.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn delete(&self) -> Result<(), Error> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::State {
                path: self.path(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample() -> StateFile {
        StateFile {
            version: 1,
            repo: "octo/widgets".into(),
            instances: vec![InstanceRecord {
                id: "i-0123".into(),
                launched_at: Utc::now(),
                expires_at: Utc::now(),
            }],
        }
    }

    #[test]
    fn write_read_roundtrip_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        assert!(rs.read().unwrap().is_none());
        rs.write(&sample()).unwrap();
        assert_eq!(rs.read().unwrap().unwrap().instances[0].id, "i-0123");
        rs.delete().unwrap();
        assert!(rs.read().unwrap().is_none());
        rs.delete().unwrap(); // idempotent
    }

    #[test]
    fn write_is_rename_based_so_a_crashed_write_leaves_old_state() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        rs.write(&sample()).unwrap();
        // Model a crash between tmp-write and rename: a half-written tmp file exists.
        std::fs::write(d.path().join("state.json.tmp"), b"{\"version\":1,\"repo").unwrap();
        let read = rs.read().unwrap().unwrap();
        assert_eq!(
            read.instances.len(),
            1,
            "reader must see the old committed state"
        );
        // And no tmp residue is ever read as state.
    }

    #[test]
    fn corrupt_statefile_is_a_loud_error_not_empty() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        std::fs::write(d.path().join("state.json"), b"not json").unwrap();
        assert!(matches!(rs.read(), Err(Error::StateCorrupt { .. })));
    }

    #[test]
    fn unknown_version_is_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        // Written directly, not via write(): write() now itself refuses to
        // persist a version it can't read back (see write_rejects_wrong_version).
        std::fs::write(
            d.path().join("state.json"),
            br#"{"version":2,"repo":"octo/widgets","instances":[]}"#,
        )
        .unwrap();
        assert!(matches!(rs.read(), Err(Error::StateCorrupt { .. })));
    }

    #[test]
    fn write_rejects_wrong_version() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        let mut s = sample();
        s.version = 2;
        assert!(matches!(rs.write(&s), Err(Error::StateCorrupt { .. })));
        // Nothing should have been persisted.
        assert!(rs.read().unwrap().is_none());
    }

    #[test]
    fn read_rejects_repo_mismatch() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState {
            dir: d.path().to_path_buf(),
            expected_repo: Some("octo/widgets".into()),
        };
        let mut s = sample();
        s.repo = "someone/else".into();
        rs.write(&s).unwrap();
        assert!(matches!(rs.read(), Err(Error::StateCorrupt { .. })));
    }

    #[test]
    fn open_respects_xdg_state_home() {
        let d = tempfile::tempdir().unwrap();
        let root = state_root_from(Some(d.path().as_os_str().into()), None).unwrap();
        assert_eq!(root, d.path().join("burst"));
        let home = state_root_from(None, Some("/home/u".into())).unwrap();
        assert_eq!(home, std::path::Path::new("/home/u/.local/state/burst"));
        assert!(state_root_from(None, None).is_err());
    }
}
