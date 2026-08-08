#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid repository {given:?}: expected owner/repo (letters, digits, . _ -)")]
    RepoInvalid { given: String },
    #[error("burst {cmd}: not implemented yet (see implementation-phases.md)")]
    NotImplemented { cmd: &'static str },
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
}
