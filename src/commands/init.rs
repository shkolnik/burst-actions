//! `burst init owner/repo`: write an annotated `burst.toml` into the current
//! directory. The only command that needs no config, no AWS and no GitHub —
//! it is what a repo runs before it has any of them.

use crate::config::{EXAMPLE, EXAMPLE_REPO_LINE};
use crate::error::Error;
use crate::schema::RepoId;
use std::path::Path;

/// The template with its placeholder repo replaced by `repo`. Everything else
/// stays commented out: the file a user edits is the documentation.
pub fn render(repo: &RepoId) -> String {
    EXAMPLE.replace(EXAMPLE_REPO_LINE, &format!("repo = \"{repo}\""))
}

pub fn run(dir: &Path, repo: &str) -> Result<(), Error> {
    let repo = RepoId::parse(repo)?;
    let path = dir.join("burst.toml");
    // Never overwrite: this file is hand-edited, and a clobbered instance_type
    // or volume_gb would surface as a surprising fleet, not as an error.
    if path.exists() {
        return Err(Error::Environment {
            reason: format!(
                "{} already exists — edit it, or delete it first",
                path.display()
            ),
        });
    }
    std::fs::write(&path, render(&repo)).map_err(|source| Error::ConfigRead {
        path: path.clone(),
        source,
    })?;
    println!(
        "wrote {} for {repo}\nedit it (every setting is documented inline), then run `burst bake`",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_a_config_that_loads_with_the_given_repo() {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), "octo/widgets").unwrap();
        let config = crate::config::load(d.path(), None).unwrap();
        assert_eq!(config.repo.to_string(), "octo/widgets");
        // Only `repo` is live: everything else stays commented, so the file
        // documents the defaults rather than pinning them.
        assert_eq!(config.instance_type, "c7i.2xlarge");
        let text = std::fs::read_to_string(d.path().join("burst.toml")).unwrap();
        assert!(text.contains("#volume_gb = 100"), "{text}");
    }

    #[test]
    fn init_refuses_to_clobber_an_existing_config() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("burst.toml"), "[burst]\nrepo = \"a/b\"\n").unwrap();
        let e = run(d.path(), "octo/widgets").unwrap_err().to_string();
        assert!(
            e.contains("burst.toml") && e.contains("already exists"),
            "{e}"
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("burst.toml")).unwrap(),
            "[burst]\nrepo = \"a/b\"\n"
        );
    }

    #[test]
    fn init_rejects_a_malformed_repo_before_writing_anything() {
        let d = tempfile::tempdir().unwrap();
        assert!(run(d.path(), "not-a-repo").is_err());
        assert!(!d.path().join("burst.toml").exists());
    }
}
