#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid repository {given:?}: expected owner/repo (letters, digits, . _ -)")]
    RepoInvalid { given: String },
    #[error("cannot read {path}: {source}", path = .path.display())]
    ConfigRead {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config {path}: {reason}", path = .path.display())]
    ConfigInvalid {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("no repository: pass --repo owner/repo or set repo in burst.toml")]
    RepoMissing,
    #[error("state file {path}: {source}", path = .path.display())]
    State {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("state file {path} is corrupt ({reason}) — if no burst fleet is live, delete it; if one is, run burst status", path = .path.display())]
    StateCorrupt {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("{reason}")]
    Environment { reason: String },
    #[error("another burst invocation is already running for this repo (lock held in {repo_dir}); a crashed run would have released it — wait for or stop the other invocation", repo_dir = .repo_dir.display())]
    LockHeld { repo_dir: std::path::PathBuf },
}
