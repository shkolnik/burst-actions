//! `burst bake`: resolve every image-key input (GitHub PAT, runner agent
//! version, pinned base AMI, rendered provisioning script), then delegate
//! the get-or-create to `Cloud::bake`.

use crate::cloud::Cloud;
use crate::config::Config;
use crate::error::Error;

pub fn run(config: &Config) -> Result<(), Error> {
    let mut p = super::image::prepare(config)?;
    let image_id = p.cloud.bake(&p.key)?;
    println!("image ready: {image_id} ({})", p.key);
    Ok(())
}
