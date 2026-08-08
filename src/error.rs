#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid repository {given:?}: expected owner/repo (letters, digits, . _ -)")]
    RepoInvalid { given: String },
    #[error("burst {cmd}: not implemented yet (see implementation-phases.md)")]
    NotImplemented { cmd: &'static str },
}
