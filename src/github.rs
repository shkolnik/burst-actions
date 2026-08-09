use crate::error::Error;
use crate::schema::RepoId;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Fleet runner name: burst-<8 hex chars>. The nonce is the ownership
/// pattern the registration sweep keys on, so the format is a contract.
pub fn runner_name(nonce: &str) -> String {
    format!("burst-{nonce}")
}

/// Unique-per-mint nonce: 8 lowercase hex chars from time ^ pid ^ counter
/// (uniqueness within a process is what matters; the counter guarantees it).
pub fn runner_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed = nanos ^ pid ^ count;
    format!("{:08x}", (mixed as u32))
}

/// True iff `name` matches the exact pattern runner_name emits:
/// ^burst-[0-9a-f]{8}$. The home runner (any human-chosen name) must never
/// match; this is the prove-ownership check before a registration delete.
pub fn is_burst_runner_name(name: &str) -> bool {
    name.strip_prefix("burst-").is_some_and(|rest| {
        rest.len() == 8
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRegistration {
    pub id: u64,
    pub name: String,
    /// GitHub's `status` == "online".
    pub online: bool,
    pub busy: bool,
}

/// Registrations safe to delete: offline, not busy, and named in `minted` —
/// the runner names this host's manifest created. A JIT runner that ran its
/// job is deregistered by GitHub automatically — what remains are
/// never-connected mints (VM died before registering). Manifest scoping is
/// what makes "offline" unambiguous: a registration we didn't mint may be a
/// concurrent invocation's still-booting runner, which is indistinguishable
/// from a corpse by the API alone, so it is simply never ours to judge.
pub fn dead_registrations<'r>(
    rs: &'r [RunnerRegistration],
    minted: &[String],
) -> Vec<&'r RunnerRegistration> {
    rs.iter()
        .filter(|r| !r.online && !r.busy && minted.iter().any(|m| m == &r.name))
        .collect()
}

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

/// Count jobs in a run's jobs payload that are queued AND labeled for burst
/// (labels contain "burst"). Pure over the API's JSON shape.
pub fn count_queued_burst_jobs(jobs_response: &serde_json::Value) -> u32 {
    jobs_response["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter(|j| j["status"].as_str() == Some("queued"))
                .filter(|j| {
                    j["labels"]
                        .as_array()
                        .is_some_and(|ls| ls.iter().any(|l| l.as_str() == Some("burst")))
                })
                .count() as u32
        })
        .unwrap_or(0)
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

    /// Ids of queued workflow runs:
    /// GET /repos/{o}/{r}/actions/runs?status=queued&per_page=100, paginated
    /// to exhaustion (the loop stops once a page returns fewer than
    /// `per_page` results).
    pub fn queued_run_ids(&self, repo: &RepoId) -> Result<Vec<u64>, Error> {
        const OP: &str = "GET .../actions/runs?status=queued";
        const PER_PAGE: usize = 100;
        let mut ids = Vec::new();
        let mut page = 1;
        loop {
            let url = format!(
                "{}/repos/{}/{}/actions/runs?status=queued&per_page={}&page={}",
                self.base_url,
                repo.owner(),
                repo.name(),
                PER_PAGE,
                page
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
            let runs = body
                .get("workflow_runs")
                .and_then(|v| v.as_array())
                .ok_or_else(|| Error::GitHub {
                    op: OP,
                    status: 200,
                    message: "response missing workflow_runs".to_string(),
                })?;
            let page_len = runs.len();
            for run in runs {
                if let Some(id) = run.get("id").and_then(|v| v.as_u64()) {
                    ids.push(id);
                }
            }
            if page_len < PER_PAGE {
                break;
            }
            page += 1;
        }
        Ok(ids)
    }

    /// The --auto fleet-size input: sum of count_queued_burst_jobs over
    /// GET /repos/{o}/{r}/actions/runs/{id}/jobs?per_page=100 for each
    /// queued run — one call per run, by API design.
    pub fn queued_burst_job_count(&self, repo: &RepoId) -> Result<u32, Error> {
        const OP: &str = "GET .../actions/runs/{id}/jobs";
        let mut total = 0;
        for run_id in self.queued_run_ids(repo)? {
            let url = format!(
                "{}/repos/{}/{}/actions/runs/{}/jobs?per_page=100",
                self.base_url,
                repo.owner(),
                repo.name(),
                run_id
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
            total += count_queued_burst_jobs(&body);
        }
        Ok(total)
    }

    /// GET /repos/{o}/{r}/actions/runners?per_page=100, paginated.
    pub fn list_runners(&self, repo: &RepoId) -> Result<Vec<RunnerRegistration>, Error> {
        const OP: &str = "GET .../actions/runners";
        const PER_PAGE: usize = 100;
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let url = format!(
                "{}/repos/{}/{}/actions/runners?per_page={}&page={}",
                self.base_url,
                repo.owner(),
                repo.name(),
                PER_PAGE,
                page
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
            let runners = body
                .get("runners")
                .and_then(|v| v.as_array())
                .ok_or_else(|| Error::GitHub {
                    op: OP,
                    status: 200,
                    message: "response missing runners".to_string(),
                })?;
            let page_len = runners.len();
            for r in runners {
                let id = r
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::GitHub {
                        op: OP,
                        status: 200,
                        message: "runner entry missing id".to_string(),
                    })?;
                let name = r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::GitHub {
                        op: OP,
                        status: 200,
                        message: "runner entry missing name".to_string(),
                    })?
                    .to_string();
                let online = r.get("status").and_then(|v| v.as_str()) == Some("online");
                let busy = r.get("busy").and_then(|v| v.as_bool()).unwrap_or(false);
                out.push(RunnerRegistration {
                    id,
                    name,
                    online,
                    busy,
                });
            }
            if page_len < PER_PAGE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// DELETE /repos/{o}/{r}/actions/runners/{id}. Re-verifies ownership at
    /// the destructive site: refuses (Error::GitHub, op "delete runner")
    /// unless is_burst_runner_name(name) — even though callers select via
    /// dead_registrations, the last check lives here. 404 is Ok (GitHub
    /// GC'd it first — the delete is idempotent).
    pub fn delete_runner(&self, repo: &RepoId, id: u64, name: &str) -> Result<(), Error> {
        const OP: &str = "delete runner";
        if !is_burst_runner_name(name) {
            return Err(Error::GitHub {
                op: OP,
                status: 0,
                message: format!("refusing to delete non-burst runner {name:?}"),
            });
        }
        let url = format!(
            "{}/repos/{}/{}/actions/runners/{}",
            self.base_url,
            repo.owner(),
            repo.name(),
            id
        );
        let resp = ureq::delete(url)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "burst")
            .config()
            .http_status_as_error(false)
            .build()
            .call();
        let resp = resp.map_err(|e| Error::GitHub {
            op: OP,
            status: 0,
            message: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        if status == 404 || (200..300).contains(&status) {
            return Ok(());
        }
        let mut resp = resp;
        let body_text = resp.body_mut().read_to_string().unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body_text)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or(body_text);
        Err(Error::GitHub {
            op: OP,
            status,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_name_formats_burst_prefix() {
        assert_eq!(runner_name("deadbeef"), "burst-deadbeef");
    }

    #[test]
    fn is_burst_runner_name_accepts_runner_name_output() {
        assert!(is_burst_runner_name(&runner_name("deadbeef")));
    }

    #[test]
    fn is_burst_runner_name_rejects_uppercase_hex() {
        assert!(!is_burst_runner_name("burst-DEADBEEF"));
    }

    #[test]
    fn is_burst_runner_name_rejects_short_suffix() {
        assert!(!is_burst_runner_name("burst-123"));
    }

    #[test]
    fn is_burst_runner_name_rejects_non_burst_name() {
        assert!(!is_burst_runner_name("my-home-runner"));
    }

    #[test]
    fn is_burst_runner_name_rejects_long_suffix() {
        assert!(!is_burst_runner_name("burst-deadbeef1"));
    }

    #[test]
    fn runner_nonce_calls_differ_and_both_accepted() {
        let a = runner_nonce();
        let b = runner_nonce();
        assert_ne!(a, b);
        assert!(is_burst_runner_name(&runner_name(&a)));
        assert!(is_burst_runner_name(&runner_name(&b)));
    }

    #[test]
    fn dead_registrations_keeps_only_offline_idle_minted_names() {
        let online_burst = RunnerRegistration {
            id: 1,
            name: runner_name("aaaaaaaa"),
            online: true,
            busy: false,
        };
        let busy_burst = RunnerRegistration {
            id: 2,
            name: runner_name("bbbbbbbb"),
            online: false,
            busy: true,
        };
        let dead_burst = RunnerRegistration {
            id: 3,
            name: runner_name("cccccccc"),
            online: false,
            busy: false,
        };
        let offline_home = RunnerRegistration {
            id: 4,
            name: "home".to_string(),
            online: false,
            busy: false,
        };
        // Minted by this manifest but not offered for deletion: online/busy.
        // Offline but never minted here (another invocation's, or the home
        // runner): out of scope entirely.
        let unminted_burst = RunnerRegistration {
            id: 5,
            name: runner_name("dddddddd"),
            online: false,
            busy: false,
        };
        let minted = vec![
            runner_name("aaaaaaaa"),
            runner_name("bbbbbbbb"),
            runner_name("cccccccc"),
        ];
        let rs = vec![
            online_burst,
            busy_burst,
            dead_burst.clone(),
            offline_home,
            unminted_burst,
        ];
        let dead = dead_registrations(&rs, &minted);
        assert_eq!(dead, vec![&dead_burst]);
    }

    #[test]
    fn delete_runner_refuses_non_burst_name_before_any_network_call() {
        let client = Client::new(Token::from("unused"));
        let repo = RepoId::parse("octo/widgets").unwrap();
        let err = client.delete_runner(&repo, 1, "home").unwrap_err();
        match err {
            Error::GitHub { op, message, .. } => {
                assert_eq!(op, "delete runner");
                assert!(message.contains("home"), "{message}");
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
    fn count_queued_burst_jobs_counts_queued_job_labeled_burst() {
        let jobs = serde_json::json!({
            "jobs": [
                {"status": "queued", "labels": ["self-hosted", "burst"]}
            ]
        });
        assert_eq!(count_queued_burst_jobs(&jobs), 1);
    }

    #[test]
    fn count_queued_burst_jobs_excludes_in_progress_job_with_label() {
        let jobs = serde_json::json!({
            "jobs": [
                {"status": "in_progress", "labels": ["self-hosted", "burst"]}
            ]
        });
        assert_eq!(count_queued_burst_jobs(&jobs), 0);
    }

    #[test]
    fn count_queued_burst_jobs_excludes_queued_job_without_burst_label() {
        let jobs = serde_json::json!({
            "jobs": [
                {"status": "queued", "labels": ["self-hosted", "home"]}
            ]
        });
        assert_eq!(count_queued_burst_jobs(&jobs), 0);
    }

    #[test]
    fn count_queued_burst_jobs_zero_on_missing_jobs_array() {
        let jobs = serde_json::json!({});
        assert_eq!(count_queued_burst_jobs(&jobs), 0);
    }

    #[test]
    fn count_queued_burst_jobs_zero_on_empty_jobs_array() {
        let jobs = serde_json::json!({"jobs": []});
        assert_eq!(count_queued_burst_jobs(&jobs), 0);
    }

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
