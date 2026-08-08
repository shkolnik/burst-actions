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
    #[error(
        "no GitHub token: set BURST_GITHUB_TOKEN (or GITHUB_TOKEN) to a fine-grained PAT with Administration read/write on the target repo"
    )]
    GitHubTokenMissing,
    #[error("GitHub API {op} failed ({status}): {message}")]
    GitHub {
        op: &'static str,
        status: u16,
        message: String,
    },
    #[error(
        "no AWS region configured: set AWS_REGION, add region to your AWS profile, or set region in burst.toml"
    )]
    RegionMissing,
    #[error("AWS {op} failed: {message}")]
    Aws { op: &'static str, message: String },
    #[error(
        "no default VPC in {region}: burst launches into the default VPC only — create one with `aws ec2 create-default-vpc --region {region}`"
    )]
    NoDefaultVpc { region: String },
    #[error(
        "bake timed out: builder {instance_id} did not reach 'stopped' within {minutes} min — provisioning likely failed; the builder was terminated and its kill schedule deleted"
    )]
    BakeTimeout { instance_id: String, minutes: u64 },
}
