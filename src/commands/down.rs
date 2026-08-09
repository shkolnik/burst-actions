//! `burst down`: tag-verified teardown of this repo's fleet. Reuses Task
//! 8's minimal read path (list only needs Cloud, not GitHub) to find the
//! fleet, confirms, terminates + disarms through the fenced Cloud methods,
//! tidies dead GitHub registrations (warning-only — instances already down
//! is the billing fact), and removes the statefile if no watcher holds the
//! lock.

use crate::cloud::Cloud;
use crate::cloud::aws::{AwsCloud, AwsContext};
use crate::config::Config;
use crate::error::Error;
use crate::github;
use crate::state::RepoState;

/// y/N confirmation, injected reader so tests never touch stdin. `--yes`
/// (`yes_flag`) bypasses without reading. Anything but "y"/"yes"
/// (case-insensitive, trimmed) is No. General on purpose: Task 11's
/// cross-host advisory prompt reuses this.
pub fn confirm(prompt: &str, yes_flag: bool, input: &mut impl std::io::BufRead) -> bool {
    if yes_flag {
        return true;
    }
    print!("{prompt}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut line = String::new();
    if input.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Terminate then disarm every id — the fenced Cloud methods re-verify
/// ownership themselves. Factored out of `run` so it can be exercised
/// against `FakeCloud` without any GitHub or statefile involvement.
pub fn teardown(cloud: &mut impl Cloud, ids: &[String]) -> Result<(), Error> {
    cloud.terminate(ids)?;
    for id in ids {
        cloud.disarm_kill(id)?;
    }
    Ok(())
}

pub fn run(config: &Config, yes: bool) -> Result<(), Error> {
    let ctx = AwsContext::connect(config.region.as_deref())?;
    let substrate = ctx.ensure_substrate(config.budget_alarm_usd)?;
    let mut cloud = AwsCloud {
        ctx,
        substrate,
        repo: config.repo.clone(),
        base_ami: String::new(),
        builder_instance_type: String::new(),
        provisioning_script: String::new(),
    };

    let instances = cloud.list_tagged(&config.repo)?;
    if instances.is_empty() {
        println!("no live fleet for {} — nothing to terminate", config.repo);
        return Ok(());
    }

    let ids: Vec<String> = instances.iter().map(|i| i.id.clone()).collect();
    let prompt = format!(
        "terminate {} instances for {}? [y/N] ",
        ids.len(),
        config.repo
    );
    let mut stdin = std::io::stdin().lock();
    if !confirm(&prompt, yes, &mut stdin) {
        println!("aborted");
        return Ok(());
    }

    teardown(&mut cloud, &ids)?;

    let repo_state = RepoState::open(&config.repo)?;
    // Registration tidy is manifest-scoped: only names this host's statefile
    // minted are candidates. No statefile (e.g. tearing down an adopted
    // fleet) ⇒ nothing here is ours to delete on GitHub.
    let minted: Vec<String> = match repo_state.read() {
        Ok(m) => m
            .map(|m| {
                m.instances
                    .iter()
                    .filter_map(|r| r.runner.clone())
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            eprintln!(
                "warning: statefile unreadable — skipping GitHub registration tidy (instances are already down): {e}"
            );
            Vec::new()
        }
    };
    super::sweep::tidy_dead_registrations(
        github::token_from_env().map(github::Client::new),
        &config.repo,
        &minted,
    );
    match repo_state.lock() {
        Ok(_lock) => repo_state.delete()?,
        Err(Error::LockHeld { .. }) => {
            println!(
                "statefile left in place: a watcher is attached from this host and will notice the fleet is gone and tidy up itself"
            );
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::fake::FakeCloud;
    use crate::cloud::{Instance, InstanceState};
    use chrono::Utc;

    fn plant(cloud: &mut FakeCloud, id: &str) {
        cloud.plant(Instance {
            id: id.to_string(),
            state: InstanceState::Running,
            tags: vec![
                ("burst-actions".to_string(), "1".to_string()),
                ("burst-actions-repo".to_string(), "octo/widgets".to_string()),
            ],
        });
    }

    #[test]
    fn yes_flag_short_circuits_without_reading() {
        // A reader that errors if read from at all.
        struct Poison;
        impl std::io::Read for Poison {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                panic!("must not read stdin when yes_flag is set");
            }
        }
        let mut r = std::io::BufReader::new(Poison);
        assert!(confirm("terminate? [y/N] ", true, &mut r));
    }

    #[test]
    fn y_and_yes_case_insensitive_are_yes() {
        for input in ["y\n", "Y\n", "yes\n", "YES\n", "Yes\n"] {
            let mut r = std::io::BufReader::new(input.as_bytes());
            assert!(confirm("? ", false, &mut r), "input {input:?} must be yes");
        }
    }

    #[test]
    fn blank_no_and_anything_else_are_no() {
        for input in ["\n", "n\n", "anything\n"] {
            let mut r = std::io::BufReader::new(input.as_bytes());
            assert!(!confirm("? ", false, &mut r), "input {input:?} must be no");
        }
    }

    #[test]
    fn teardown_terminates_and_disarms_all() {
        let mut cloud = FakeCloud::default();
        plant(&mut cloud, "i-aaa");
        plant(&mut cloud, "i-bbb");
        let repo = crate::schema::RepoId::parse("octo/widgets").unwrap();
        let ids: Vec<String> = cloud
            .list_tagged(&repo)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        cloud.arm_kill(&ids[0], Utc::now()).unwrap();
        cloud.arm_kill(&ids[1], Utc::now()).unwrap();

        teardown(&mut cloud, &ids).unwrap();

        assert!(cloud.list_tagged(&repo).unwrap().is_empty());
        assert!(cloud.armed_kills().is_empty());
    }
}
