use crate::error::Error;
use crate::schema::RepoId;
use std::fmt;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// A GitHub PAT. `Debug` never prints the secret — logs, panics, and error
/// messages built from `{:?}` cannot leak it.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Token {
    fn from(s: &str) -> Self {
        Token(s.to_string())
    }
}

impl From<String> for Token {
    fn from(s: String) -> Self {
        Token(s)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token(***)")
    }
}

/// `BURST_GITHUB_TOKEN` takes precedence over `GITHUB_TOKEN`; injected lookup
/// so tests never touch real process env.
pub fn token_from(vars: impl Fn(&str) -> Option<String>) -> Result<Token, Error> {
    vars("BURST_GITHUB_TOKEN")
        .or_else(|| vars("GITHUB_TOKEN"))
        .map(Token::from)
        .ok_or(Error::GitHubTokenMissing)
}

pub fn token_from_env() -> Result<Token, Error> {
    token_from(|name| std::env::var(name).ok())
}

/// `v2.328.0` -> `2.328.0`; idempotent when no leading `v` is present.
pub fn strip_release_tag(tag: &str) -> String {
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

/// Body for `POST .../actions/runners/generate-jitconfig`.
pub fn jit_request_body(runner_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": runner_name,
        "runner_group_id": 1,
        "labels": ["self-hosted", "burst"],
        "work_folder": "_work",
    })
}

/// The repo's "approval for fork pull-request workflows" policy — a closed
/// set; an unrecognized API value is a loud error, never a permissive
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkApprovalPolicy {
    /// Every outside collaborator's workflow needs approval — the only
    /// policy burst accepts.
    AllExternalContributors,
    FirstTimeContributors,
    FirstTimeContributorsNewToGitHub,
}

pub fn parse_approval_policy(s: &str) -> Result<ForkApprovalPolicy, Error> {
    match s {
        "all_external_contributors" => Ok(ForkApprovalPolicy::AllExternalContributors),
        "first_time_contributors" => Ok(ForkApprovalPolicy::FirstTimeContributors),
        "first_time_contributors_new_to_github" => {
            Ok(ForkApprovalPolicy::FirstTimeContributorsNewToGitHub)
        }
        other => Err(Error::GitHub {
            op: "parse approval_policy",
            status: 200,
            message: format!("unrecognized approval_policy value: {other:?}"),
        }),
    }
}

/// Invariant 5. Hard error unless the policy is AllExternalContributors.
/// There is deliberately no bypass parameter — the signature cannot express
/// "skip the check".
pub fn preflight_fork_approval(repo: &RepoId, policy: ForkApprovalPolicy) -> Result<(), Error> {
    match policy {
        ForkApprovalPolicy::AllExternalContributors => Ok(()),
        ForkApprovalPolicy::FirstTimeContributors => Err(Error::ForkApprovalTooWeak {
            repo: repo.to_string(),
            found: "only first-time contributors need approval".to_string(),
        }),
        ForkApprovalPolicy::FirstTimeContributorsNewToGitHub => Err(Error::ForkApprovalTooWeak {
            repo: repo.to_string(),
            found: "only first-time contributors new to GitHub need approval".to_string(),
        }),
    }
}

pub struct Client {
    token: Token,
    base_url: String,
}

impl Client {
    pub fn new(token: Token) -> Self {
        Client::with_base_url(token, DEFAULT_BASE_URL.to_string())
    }

    pub fn with_base_url(token: Token, base_url: String) -> Self {
        Client { token, base_url }
    }

    /// Reads a JSON body if the response is 2xx; otherwise turns the response
    /// (status + `message` field, or raw text) into `Error::GitHub`, with
    /// 401/403 reworded per design decision 8.
    fn read_ok_json(
        op: &'static str,
        resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<serde_json::Value, Error> {
        let mut resp = resp.map_err(|e| Error::GitHub {
            op,
            status: 0,
            message: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = resp.body_mut().read_to_string().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or(body_text);
            let message = if status == 401 || status == 403 {
                format!("token invalid or expired — rotate it ({message})")
            } else {
                message
            };
            return Err(Error::GitHub {
                op,
                status,
                message,
            });
        }
        resp.body_mut().read_json().map_err(|e| Error::GitHub {
            op,
            status,
            message: format!("unparseable response: {e}"),
        })
    }

    pub fn runner_agent_version(&self) -> Result<String, Error> {
        const OP: &str = "GET /repos/actions/runner/releases/latest";
        let url = format!("{}/repos/actions/runner/releases/latest", self.base_url);
        let resp = ureq::get(url)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "burst")
            .config()
            .http_status_as_error(false)
            .build()
            .call();
        let body = Self::read_ok_json(OP, resp)?;
        let tag = body
            .get("tag_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::GitHub {
                op: OP,
                status: 200,
                message: "response missing tag_name".to_string(),
            })?;
        Ok(strip_release_tag(tag))
    }

    pub fn mint_jit_config(&self, repo: &RepoId, runner_name: &str) -> Result<String, Error> {
        const OP: &str = "POST .../actions/runners/generate-jitconfig";
        let url = format!(
            "{}/repos/{}/{}/actions/runners/generate-jitconfig",
            self.base_url,
            repo.owner(),
            repo.name()
        );
        let body = jit_request_body(runner_name);
        let resp = ureq::post(url)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "burst")
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(&body);
        let json = Self::read_ok_json(OP, resp)?;
        json.get("encoded_jit_config")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::GitHub {
                op: OP,
                status: 200,
                message: "response missing encoded_jit_config".to_string(),
            })
    }

    /// GET /repos/{owner}/{repo}/actions/permissions/fork-pr-contributor-approval
    pub fn fork_approval_policy(&self, repo: &RepoId) -> Result<ForkApprovalPolicy, Error> {
        const OP: &str = "GET .../actions/permissions/fork-pr-contributor-approval";
        let url = format!(
            "{}/repos/{}/{}/actions/permissions/fork-pr-contributor-approval",
            self.base_url,
            repo.owner(),
            repo.name()
        );
        let resp = ureq::get(url)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "burst")
            .config()
            .http_status_as_error(false)
            .build()
            .call();
        let body = Self::read_ok_json(OP, resp)?;
        let value = body
            .get("approval_policy")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::GitHub {
                op: OP,
                status: 200,
                message: "response missing approval_policy".to_string(),
            })?;
        parse_approval_policy(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jit_request_body_has_exact_shape() {
        let body = jit_request_body("burst-runner-abc");
        let obj = body.as_object().unwrap();
        assert_eq!(obj.len(), 4, "body must have exactly four keys: {body}");
        assert_eq!(obj["name"], "burst-runner-abc");
        assert_eq!(obj["runner_group_id"], 1);
        assert_eq!(obj["labels"], serde_json::json!(["self-hosted", "burst"]));
        assert_eq!(obj["work_folder"], "_work");
    }

    #[test]
    fn strip_release_tag_removes_leading_v() {
        assert_eq!(strip_release_tag("v2.328.0"), "2.328.0");
    }

    #[test]
    fn strip_release_tag_idempotent() {
        assert_eq!(strip_release_tag("2.328.0"), "2.328.0");
    }

    #[test]
    fn token_debug_redacts_secret() {
        let t = Token::from("ghp_supersecretvalue");
        let printed = format!("{t:?}");
        assert!(!printed.contains("ghp_supersecretvalue"));
        assert_eq!(printed, "Token(***)");
    }

    #[test]
    fn token_from_prefers_burst_specific_var() {
        let t = token_from(|name| match name {
            "BURST_GITHUB_TOKEN" => Some("from-burst".to_string()),
            "GITHUB_TOKEN" => Some("from-generic".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(t.as_str(), "from-burst");
    }

    #[test]
    fn token_from_falls_back_to_generic_var() {
        let t = token_from(|name| match name {
            "GITHUB_TOKEN" => Some("from-generic".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(t.as_str(), "from-generic");
    }

    #[test]
    fn token_from_errors_when_both_missing() {
        let err = token_from(|_| None).unwrap_err();
        assert!(matches!(err, Error::GitHubTokenMissing));
    }

    #[test]
    fn parse_approval_policy_maps_all_three_strings() {
        assert_eq!(
            parse_approval_policy("all_external_contributors").unwrap(),
            ForkApprovalPolicy::AllExternalContributors
        );
        assert_eq!(
            parse_approval_policy("first_time_contributors").unwrap(),
            ForkApprovalPolicy::FirstTimeContributors
        );
        assert_eq!(
            parse_approval_policy("first_time_contributors_new_to_github").unwrap(),
            ForkApprovalPolicy::FirstTimeContributorsNewToGitHub
        );
    }

    #[test]
    fn parse_approval_policy_errors_on_unrecognized_value_naming_it() {
        let err = parse_approval_policy("totally_new_value").unwrap_err();
        match err {
            Error::GitHub {
                op,
                status,
                message,
            } => {
                assert_eq!(op, "parse approval_policy");
                assert_eq!(status, 200);
                assert!(message.contains("totally_new_value"));
            }
            other @ (Error::RepoInvalid { .. }
            | Error::ConfigRead { .. }
            | Error::ConfigInvalid { .. }
            | Error::RepoMissing
            | Error::State { .. }
            | Error::StateCorrupt { .. }
            | Error::Environment { .. }
            | Error::LockHeld { .. }
            | Error::GitHubTokenMissing
            | Error::RegionMissing
            | Error::Aws { .. }
            | Error::NoDefaultVpc { .. }
            | Error::BakeTimeout { .. }
            | Error::ForkApprovalTooWeak { .. }
            | Error::NoDefaultSubnet { .. }
            | Error::PartialLaunch { .. }) => panic!("expected Error::GitHub, got {other:?}"),
        }
    }

    #[test]
    fn preflight_fork_approval_passes_only_all_external_contributors() {
        let repo = RepoId::parse("octo/widgets").unwrap();
        assert!(
            preflight_fork_approval(&repo, ForkApprovalPolicy::AllExternalContributors).is_ok()
        );
        assert!(preflight_fork_approval(&repo, ForkApprovalPolicy::FirstTimeContributors).is_err());
        assert!(
            preflight_fork_approval(&repo, ForkApprovalPolicy::FirstTimeContributorsNewToGitHub)
                .is_err()
        );
    }

    #[test]
    fn preflight_fork_approval_rejection_names_repo_and_finding_and_remedy() {
        let repo = RepoId::parse("octo/widgets").unwrap();
        for policy in [
            ForkApprovalPolicy::FirstTimeContributors,
            ForkApprovalPolicy::FirstTimeContributorsNewToGitHub,
        ] {
            let err = preflight_fork_approval(&repo, policy).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("octo/widgets"), "{message}");
            assert!(
                message.contains("Require approval for all external contributors"),
                "{message}"
            );
            match &err {
                Error::ForkApprovalTooWeak { found, .. } => {
                    assert!(!found.is_empty(), "{message}");
                }
                other @ (Error::RepoInvalid { .. }
                | Error::ConfigRead { .. }
                | Error::ConfigInvalid { .. }
                | Error::RepoMissing
                | Error::State { .. }
                | Error::StateCorrupt { .. }
                | Error::Environment { .. }
                | Error::LockHeld { .. }
                | Error::GitHubTokenMissing
                | Error::GitHub { .. }
                | Error::RegionMissing
                | Error::Aws { .. }
                | Error::NoDefaultVpc { .. }
                | Error::BakeTimeout { .. }
                | Error::NoDefaultSubnet { .. }
                | Error::PartialLaunch { .. }) => {
                    panic!("expected Error::ForkApprovalTooWeak, got {other:?}")
                }
            }
        }
    }
}
