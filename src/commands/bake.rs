//! `burst bake`: resolve every image-key input (GitHub PAT, runner agent
//! version, pinned base AMI, rendered provisioning script), then delegate
//! the get-or-create to `Cloud::bake`.

use crate::cloud::Cloud;
use crate::cloud::aws::{AwsCloud, AwsContext};
use crate::config::Config;
use crate::error::Error;
use crate::github;
use crate::payload::render_provision;
use crate::schema::{ImageKeyInputs, image_key};

pub fn run(config: &Config) -> Result<(), Error> {
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
            let resolved = ctx.resolve_latest_ubuntu_ami(config.arch)?;
            return Err(Error::Environment {
                reason: format!(
                    "no base_ami pinned: set base_ami = \"{resolved}\" in burst.toml (current Ubuntu 24.04 LTS {arch} in {region})",
                    arch = config.arch.as_str(),
                    region = ctx.region_str(),
                ),
            });
        }
    };

    let rendered = render_provision(config.idle_timeout_min, config.ttl_hours, &agent_version)?;
    let key = image_key(&ImageKeyInputs {
        provisioning_script: rendered.as_bytes(),
        base_image_id: &base_ami,
        arch: config.arch,
        runner_agent_version: &agent_version,
    });

    let mut cloud = AwsCloud {
        ctx,
        substrate,
        repo: config.repo.clone(),
        base_ami,
        builder_instance_type: config.instance_type.clone(),
        provisioning_script: rendered,
    };
    let image_id = cloud.bake(&key)?;
    println!("image ready: {image_id} ({key})");
    Ok(())
}
