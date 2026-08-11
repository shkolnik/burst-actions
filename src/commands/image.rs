//! Shared pre-fleet sequence: resolve every image-key input (GitHub PAT,
//! runner agent version, pinned base AMI, rendered provisioning script) and
//! connect to AWS. Used by both `bake` (which calls `Cloud::bake`
//! immediately) and `up` (which calls it after sweep-on-entry and the
//! fork-approval preflight).

use crate::cloud::aws::{AwsCloud, AwsContext};
use crate::config::Config;
use crate::error::Error;
use crate::github;
use crate::payload::render_provision;
use crate::schema::{ImageKeyInputs, image_key};

/// Everything up/bake share before any fleet decision: GitHub client +
/// resolved agent version (GitHub first — a PAT problem aborts before any
/// AWS resource exists), then the connected AwsCloud and the image-cache
/// key. Does NOT call Cloud::bake — callers decide when (bake: immediately;
/// up: after sweep-on-entry and the fork-approval preflight).
pub struct Prepared {
    pub client: crate::github::Client,
    pub cloud: AwsCloud,
    pub key: String,
}

pub fn prepare(config: &Config) -> Result<Prepared, Error> {
    // GitHub first: fail before any AWS resource exists, so a PAT problem
    // (or GitHub being down) aborts clean with nothing to clean up.
    let token = github::token_from_env()?;
    let client = github::Client::new(token);
    let agent_version = client.runner_agent_version()?;

    let ctx = AwsContext::connect(config.region.as_deref())?;
    let substrate = ctx.ensure_substrate(config.budget_alarm_usd)?;

    // base_ami is the pin (§8.6): absent is fail-loud with a copy-pasteable
    // remedy naming the AMI `burst bake` would otherwise have guessed.
    let base_ami = match &config.base_ami {
        Some(ami) => ami.clone(),
        None => {
            let resolved = ctx.resolve_latest_debian_ami(config.arch)?;
            return Err(Error::Environment {
                reason: format!(
                    "no base_ami pinned: set base_ami = \"{resolved}\" in burst.toml (current Debian 13 {arch} in {region})",
                    arch = config.arch.as_str(),
                    region = ctx.region_str(),
                ),
            });
        }
    };

    // Custom provision script (burst.toml `provision`): read every run, even
    // on the cache-hit path — its bytes are an image-key input, so an edit
    // must miss the cache and rebake rather than reuse a stale AMI.
    let custom = match &config.provision {
        Some(path) => Some(
            std::fs::read_to_string(path).map_err(|source| Error::ConfigRead {
                path: path.clone(),
                source,
            })?,
        ),
        None => None,
    };
    let rendered = render_provision(&agent_version, custom.as_deref())?;
    let key = image_key(&ImageKeyInputs {
        provisioning_script: rendered.as_bytes(),
        base_image_id: &base_ami,
        arch: config.arch,
        runner_agent_version: &agent_version,
    });

    let cloud = AwsCloud {
        ctx,
        substrate,
        repo: config.repo.clone(),
        base_ami,
        builder_instance_type: config.instance_type.clone(),
        provisioning_script: rendered,
    };

    Ok(Prepared { client, cloud, key })
}
