//! Situational setup guidance for `burst --help` (and the bare `burst`, which
//! renders the same help): the steps this directory still needs, and nothing
//! about the ones it has already done. A ready directory gets no banner.

use crate::github;
use std::path::Path;

/// The steps left, or `None` when nothing is missing. Rendered after the
/// command list, so it is the last thing on screen — next to where the user
/// is about to type — and survives a short terminal's scroll.
pub fn hint(token_set: bool, config_present: bool) -> Option<String> {
    let mut steps: Vec<&str> = Vec::new();
    if !token_set {
        steps.push(
            "  no GitHub token in the environment:\n\
             \x20   export BURST_GITHUB_TOKEN=<fine-grained PAT, Administration read/write on the repo>",
        );
    }
    if !config_present {
        steps.push(
            "  no burst.toml in this directory:\n\
             \x20   burst init owner/repo",
        );
    }
    if steps.is_empty() {
        return None;
    }
    Some(format!("Setup:\n{}", steps.join("\n")))
}

/// Probe the environment with the *same* checks the commands make — the token
/// lookup from `github` (which accepts `GITHUB_TOKEN` too) and the config file
/// `config::load` reads — so the help can never nag about a step that would
/// actually have worked, or stay silent about one that wouldn't.
pub fn probe(dir: &Path) -> Option<String> {
    hint(
        github::token_from_env().is_ok(),
        dir.join("burst.toml").exists(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_directory_gets_no_banner() {
        assert_eq!(hint(true, true), None);
    }

    #[test]
    fn each_step_appears_only_while_it_is_outstanding() {
        let both = hint(false, false).unwrap();
        assert!(both.contains("export BURST_GITHUB_TOKEN=") && both.contains("burst init"));

        let no_config = hint(true, false).unwrap();
        assert!(no_config.contains("burst init"));
        assert!(
            !no_config.contains("BURST_GITHUB_TOKEN"),
            "a set token must not be re-suggested: {no_config}"
        );

        let no_token = hint(false, true).unwrap();
        assert!(no_token.contains("export BURST_GITHUB_TOKEN="));
        assert!(
            !no_token.contains("burst init"),
            "an existing config must not be re-suggested: {no_token}"
        );
    }

    /// Every line is either the heading, a reason, or a command to paste —
    /// nothing to reformat before running it.
    #[test]
    fn steps_are_copy_pasteable() {
        let both = hint(false, false).unwrap();
        assert!(both.starts_with("Setup:\n"), "{both}");
        assert!(both.contains("\n    export "), "{both}");
        assert!(both.contains("\n    burst init owner/repo"), "{both}");
    }
}
