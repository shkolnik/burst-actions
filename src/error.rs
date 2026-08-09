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
        "bake timed out: builder {instance_id} did not reach 'stopped' within {minutes} min — provisioning likely failed"
    )]
    BakeTimeout { instance_id: String, minutes: u64 },
    #[error(
        "fork pull-request workflows on {repo} do not require approval for all outside collaborators ({found}); burst refuses to launch runners — a fork can edit runs-on:, so labels are not a trust boundary. Fix: repo Settings → Actions → General → Fork pull request workflows → \"Require approval for all external contributors\""
    )]
    ForkApprovalTooWeak { repo: String, found: String },
    #[error(
        "default VPC {vpc_id} in {region} has no default subnet: create one with `aws ec2 create-default-subnet --availability-zone <az> --region {region}` (repeat per AZ as needed)"
    )]
    NoDefaultSubnet { region: String, vpc_id: String },
    #[error(
        "launched {launched} of {requested} instances, then: {message} — the launched fleet is tagged, kill-armed, and recorded; it will drain and self-terminate. Re-run `burst up` to re-attach, `burst down` to tear down"
    )]
    PartialLaunch {
        launched: u32,
        requested: u32,
        message: String,
    },
}
