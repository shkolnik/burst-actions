# Phase 3: Fleet Lifecycle & Kill-Testing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** The product: `burst up`'s full loop per design §3 (lock/adopt → substrate →
sweep-on-entry → fork-approval preflight → AMI ensure → mint N → launch N → arm N one-shots →
statefile → watch, with Ctrl-C detach and SIGKILL leaking nothing), plus `down`, `status`,
`sweep`, the remaining GitHub client (fork-approval preflight, `--auto` label-filtered queue
count, never-connected registration cleanup), adoption/resume against real cloud with the
cross-host advisory prompt — ending at the kill-test matrix gate where every cleanup layer is
*watched* firing.

**Architecture:** The `Cloud` trait stays **sync** (phase-2 decision; the watcher is a poll
loop, nothing needs async). The trait grows three methods this phase (`disarm_kill`,
`list_armed_kills`, `list_all_tagged`) — an internal, reversible seam; both backends implement
all of them and the fake remains the offline oracle. Command orchestration lives in
`src/commands/*` as thin shells over pure, unit-tested cores (sweep planning, fleet sizing,
quota capping, status rendering, registration selection) that take `&mut impl Cloud` or plain
data, so every decision is testable offline against `FakeCloud`; live-AWS/GitHub behavior is
verified by the lead at the gate, never in `cargo test`. Error messages keep one authoring
site each; every destructive operation re-verifies `burst-actions=1` ownership immediately
before acting (instance terminate already does; schedule/registration deletes go through
name-pattern ownership checks defined here). Three deferred phase-2 minors are fixed where
they naturally sit (Task 2); the fourth (preflight error copy) is absorbed into Task 3's
error text.

**Tech Stack:** existing phase-2 stack plus: `ctrlc` 3 (Ctrl-C detach flag),
`aws-sdk-servicequotas` 1 (decision 9's vCPU-quota check). No other new dependencies.

## Global Constraints

- **Never weaken invariants 3/4/5**: no cleanup path may depend on the watcher/CLI being
  alive — the watch loop only *observes*; the PAT never reaches a VM in any form — a VM gets
  exactly one single-use JIT config in user-data; the fork-approval preflight is a **hard
  error** with no bypass flag, ever (not even `--yes`).
- **Real money discipline**: every instance created — including by-hand gate experiments —
  carries the tag triple *and* an armed kill schedule from launch. Gate testing uses the
  smallest viable type (`t3.micro`); every working session ends with a sweep-equivalent
  listing (command given in the Gate task) showing zero live instances, reported as done.
- Tags exactly (constants in `src/schema.rs`): `burst-actions=1`,
  `burst-actions-repo=<owner/repo>`, `burst-actions-expires=<ISO8601>`. AWS resource names
  `burst-actions-*`; kill schedules `burst-actions-<instance-id>`. No new tag keys this phase.
- **Prove ownership before destroy**: instance terminates re-verify `burst-actions=1` on the
  specific resource immediately before acting (already enforced in `AwsCloud::terminate` —
  every new path goes through it); schedule deletes act only on names matching
  `burst-actions-i-*`; GitHub runner deletes act only on registrations whose name matches the
  burst runner-name pattern **and** whose status is offline.
- Closed sets are exhaustively-matched enums, never strings; no `_ =>` arm on our own enums.
  New alternatives introduced this phase (`ForkApprovalPolicy`, `SweepAction`) are enums.
- Fail loud, never degrade: an expired PAT, a weak fork-approval setting, a quota probe
  failure, a launch failure mid-fleet — each names what happened and the remedy; never a
  silent partial result. When an error names a remedy, verify the remedy works before
  shipping the message.
- All four checks before **every** commit:
  `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`.
- Commit as you go: one verified change per commit, honest messages, trailer lines copied
  from `git log -1`.
- §8 defaults as written: idle 10 min, TTL 6 h, `max_fleet` 12, on-demand by default with
  `--spot` opt-in, fail-loud on missing default VPC, pinned base AMI.
- `cargo test` must pass with **no** AWS credentials, **no** GitHub token, and **no**
  network: nothing offline may construct a live client eagerly or read cloud env at test
  time.
- New AWS API calls must stay inside the IAM policy in `docs/permissions.md`; the one
  deliberate addition this phase (`servicequotas:GetServiceQuota`, Task 8) is a **flagged
  scope growth for James**, recorded in that doc via a reviewed commit — never a silent
  edit.

---

### Task 1: Trait growth, LaunchSpec ssh_key, error variants, fake parity

**Files:**
- Modify: `src/cloud/mod.rs`, `src/cloud/fake.rs`, `src/cloud/aws.rs`, `src/error.rs`,
  `Cargo.toml`

**Interfaces:**
- Produces — the `Cloud` trait gains exactly three methods (defined here, referenced
  consistently by every later task):

```rust
    /// Delete the one-shot kill schedule for `instance_id`. Already-gone
    /// (fired and self-deleted, or never armed) is Ok — disarming is
    /// idempotent.
    fn disarm_kill(&mut self, instance_id: &str) -> Result<(), Error>;
    /// Instance ids that currently have an armed kill schedule
    /// (burst-actions-<id>), across all repos.
    fn list_armed_kills(&self) -> Result<Vec<String>, Error>;
    /// All non-terminated instances carrying burst-actions=1, ANY repo.
    /// The sweep's orphan-schedule check must see other repos' live
    /// instances, or it would mistake their schedules for orphans.
    fn list_all_tagged(&self) -> Result<Vec<Instance>, Error>;
```

- `LaunchSpec` gains `pub ssh_key: Option<String>` (EC2 key-pair name; `None` = no SSH,
  the default). Compiler finds every construction site.
- New `error::Error` variants (one authoring site each; `{policy}`/`{setting}` wording is
  finalized in Task 3, the variant shape lands here):

```rust
    #[error("fork pull-request workflows on {repo} do not require approval for all outside collaborators ({found}); burst refuses to launch runners — a fork can edit runs-on:, so labels are not a trust boundary. Fix: repo Settings → Actions → General → Fork pull request workflows → \"Require approval for all external contributors\"")]
    ForkApprovalTooWeak { repo: String, found: String },
    #[error("default VPC {vpc_id} in {region} has no default subnet: create one with `aws ec2 create-default-subnet --availability-zone <az> --region {region}` (repeat per AZ as needed)")]
    NoDefaultSubnet { region: String, vpc_id: String },
    #[error("launched {launched} of {requested} instances, then: {message} — the launched fleet is tagged, kill-armed, and recorded; it will drain and self-terminate. Re-run `burst up` to re-attach, `burst down` to tear down")]
    PartialLaunch { launched: u32, requested: u32, message: String },
```

- `FakeCloud` implements the three methods and gains test hooks:

```rust
    fn disarm_kill(&mut self, instance_id: &str) -> Result<(), Error> {
        self.kills.retain(|(id, _)| id != instance_id);
        Ok(())
    }
    fn list_armed_kills(&self) -> Result<Vec<String>, Error> {
        Ok(self.kills.iter().map(|(id, _)| id.clone()).collect())
    }
    fn list_all_tagged(&self) -> Result<Vec<Instance>, Error> {
        Ok(self
            .instances
            .iter()
            .filter(|i| i.state != InstanceState::Terminated)
            .filter(|i| i.tags.iter().any(|(k, v)| k == TAG_BURST && v == "1"))
            .cloned()
            .collect())
    }
```

- `AwsCloud`:
  - `disarm_kill` = the existing private `delete_kill_schedule` promoted to the trait impl
    (the private fn's body moves; internal callers switch to the trait method — one
    authoring site for schedule deletion).
  - `list_armed_kills`: `ListSchedules` with `name_prefix("burst-actions-i-")`, group
    `default`, paginated to exhaustion; map each name through the pure inverse of
    `kill_schedule_name`:

```rust
/// Inverse of kill_schedule_name: burst-actions-i-0abc -> i-0abc. None for
/// any name not carrying the burst-actions- prefix + an i- instance id —
/// never guess about foreign schedules.
pub(crate) fn instance_id_from_schedule_name(name: &str) -> Option<String> {
    name.strip_prefix("burst-actions-")
        .filter(|rest| rest.starts_with("i-"))
        .map(str::to_string)
}
```

  - `list_all_tagged`: same `DescribeInstances` as `list_tagged` minus the
    `tag:burst-actions-repo` filter (same pagination, same state filter, same exhaustive
    state mapping).
- `Cargo.toml` gains `ctrlc = "3"` and `aws-sdk-servicequotas = "1"` (used in Tasks 8/10;
  declared here so the dependency commit is one place).

**Steps:**

- [ ] **Step 1:** Failing unit tests first:
  - `src/cloud/fake.rs`: `disarm_kill_removes_only_that_schedule` (arm two, disarm one,
    `armed_kills()` has the other); `disarm_kill_is_idempotent` (disarm twice, Ok both);
    `list_all_tagged_spans_repos_but_requires_burst_tag` (launch for two repos + `plant` an
    untagged instance; all-tagged returns both repos' instances, never the untagged one).
  - `src/cloud/aws.rs`: `instance_id_from_schedule_name` round-trips
    `kill_schedule_name("i-0abc")`; returns `None` for `"burst-actions-somethingelse"` and
    `"other-i-0abc"`.
- [ ] **Step 2:** Add the trait methods, `ssh_key` field (thread it into
  `AwsCloud::launch` as `.key_name(k)` when `Some`; `FakeCloud` ignores it), error
  variants, and both impls. Fix every compiler-named construction/match site.
- [ ] **Step 3:** All four checks; commit —
  `feat: Cloud trait grows disarm_kill/list_armed_kills/list_all_tagged; LaunchSpec ssh_key; phase-3 error variants`.

---

### Task 2: Phase-2 minors — subnet error, Debian daily filter, bake double-terminate

Three recorded phase-2 findings, fixed at their natural sites before new code builds on
them.

**Files:**
- Modify: `src/cloud/aws.rs`, `src/error.rs` (message text only if needed)

**Interfaces:**
- `AwsContext::default_vpc_and_subnet`: the *second* failure site (VPC found, zero
  default-for-AZ subnets) currently returns `NoDefaultVpc` — a wrong diagnosis with a wrong
  remedy. It now returns `Error::NoDefaultSubnet { region, vpc_id }` (Task 1's variant).
  The first site (no default VPC at all) keeps `NoDefaultVpc`.
- `resolve_latest_debian_ami`: the `debian-13-{arch}-*` name filter also matches Debian's
  `daily` builds; "newest by creation date" therefore suggests a daily. Selection becomes a
  pure function so the exclusion is pinned by test:

```rust
/// Pick the newest official (non-daily) image: excludes any name containing
/// "daily", then newest creation date (ISO8601, so lexicographic order is
/// chronological).
pub(crate) fn pick_latest_debian<'a>(
    images: &'a [(String, String, String)], // (image_id, creation_date, name)
) -> Option<&'a String> {
    images
        .iter()
        .filter(|(_, _, name)| !name.contains("daily"))
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(id, _, _)| id)
}
```

  The live path collects `(id, creation_date, name)` and delegates.
- Bake double-terminate: `wait_for_stopped` currently terminates + disarms on timeout, and
  then `bake`'s error arm terminates *again* via `builder_cleanup_error` — two authoring
  sites for one cleanup. Fix the class: `wait_for_stopped` **only observes** (returns
  `Error::BakeTimeout` without touching the builder); `bake`'s single error arm remains the
  one cleanup site and now also best-effort `disarm_kill`s the builder after a successful
  terminate. `Error::BakeTimeout`'s message drops its cleanup claim (it can no longer
  promise what a different layer does):
  `"bake timed out: builder {instance_id} did not reach 'stopped' within {minutes} min — provisioning likely failed"`
  (the surrounding `builder_cleanup_error` message is what reports the cleanup outcome,
  truthfully in both branches, as its existing tests already pin).

**Steps:**

- [ ] **Step 1:** Failing unit tests: `pick_latest_debian` skips a newer `daily` in favor
  of an older release image; returns `None` when only dailies exist; picks the
  lexicographically-newest date among releases. For the double-terminate: extend
  `builder_cleanup_error` tests only if wording changes; the structural fix is proven by
  reading `wait_for_stopped`'s new body (no terminate call) plus the existing
  `builder_cleanup_error_*` tests still green — and by Gate row G4 live.
- [ ] **Step 2:** Implement all three fixes; update the `BakeTimeout` message text in
  `src/error.rs`.
- [ ] **Step 3:** All four checks; commit —
  `fix: NoDefaultSubnet diagnosis, Debian resolver skips daily builds, bake timeout cleanup has one authoring site`.

---

### Task 3: GitHub — fork-approval preflight (invariant 5, hard error)

**Files:**
- Modify: `src/github.rs`

**Interfaces:**
- Produces:

```rust
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

pub fn parse_approval_policy(s: &str) -> Result<ForkApprovalPolicy, Error>
// "all_external_contributors" | "first_time_contributors" |
// "first_time_contributors_new_to_github"; anything else ->
// Error::GitHub { op: "parse approval_policy", status: 200, message } naming
// the unrecognized value.

impl Client {
    /// GET /repos/{owner}/{repo}/actions/permissions/fork-pr-contributor-approval
    /// -> the `approval_policy` field, parsed.
    pub fn fork_approval_policy(&self, repo: &RepoId) -> Result<ForkApprovalPolicy, Error>
}

/// Invariant 5. Hard error unless the policy is AllExternalContributors.
/// There is deliberately no bypass parameter — the signature cannot express
/// "skip the check".
pub fn preflight_fork_approval(
    repo: &RepoId,
    policy: ForkApprovalPolicy,
) -> Result<(), Error> {
    match policy {
        ForkApprovalPolicy::AllExternalContributors => Ok(()),
        ForkApprovalPolicy::FirstTimeContributors => Err(Error::ForkApprovalTooWeak {
            repo: repo.to_string(),
            found: "only first-time contributors need approval".to_string(),
        }),
        ForkApprovalPolicy::FirstTimeContributorsNewToGitHub => {
            Err(Error::ForkApprovalTooWeak {
                repo: repo.to_string(),
                found: "only first-time contributors new to GitHub need approval"
                    .to_string(),
            })
        }
    }
}
```

- The endpoint path and the three `approval_policy` strings are external facts cached from
  GitHub's REST docs — **verified live at Gate row G6** (and the error copy's Settings-path
  remedy verified by following it on the real repo, per "verify the remedy"). If the live
  API differs, fix the constant + tests and record the real values.

**Steps:**

- [ ] **Step 1:** Failing unit tests: `parse_approval_policy` maps all three strings;
  errors on `"totally_new_value"` naming it; `preflight_fork_approval` passes only
  `AllExternalContributors` and each rejection message names the repo, states what was
  found, and contains `"Require approval for all external contributors"` (the exact
  setting name — the remedy must be findable in the UI).
- [ ] **Step 2:** Implement `fork_approval_policy` with the same header/error plumbing as
  the existing calls (`read_ok_json`).
- [ ] **Step 3:** All four checks; commit —
  `feat: fork-approval preflight — closed policy enum, hard error, no bypass (invariant 5)`.

---

### Task 4: GitHub — `--auto` label-filtered queued-job count

One API call **per queued run** (design §5: labels are only visible per-job; no single-call
answer exists and the code must not pretend one does).

**Files:**
- Modify: `src/github.rs`

**Interfaces:**
- Produces:

```rust
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

impl Client {
    /// Ids of queued workflow runs:
    /// GET /repos/{o}/{r}/actions/runs?status=queued&per_page=100, paginated
    /// to exhaustion (follow `total_count` vs page size).
    pub fn queued_run_ids(&self, repo: &RepoId) -> Result<Vec<u64>, Error>;
    /// The --auto fleet-size input: sum of count_queued_burst_jobs over
    /// GET /repos/{o}/{r}/actions/runs/{id}/jobs?per_page=100 for each
    /// queued run — one call per run, by API design.
    pub fn queued_burst_job_count(&self, repo: &RepoId) -> Result<u32, Error>;
}
```

**Steps:**

- [ ] **Step 1:** Failing unit tests on `count_queued_burst_jobs` with literal JSON
  fixtures: counts a queued job labeled `["self-hosted","burst"]`; excludes an
  `in_progress` job with the label; excludes a queued job labeled
  `["self-hosted","home"]`; `0` on an empty/missing `jobs` array.
- [ ] **Step 2:** Implement the two client methods (`read_ok_json` plumbing; pagination
  loop mirrors none existing yet — keep it a plain `page` counter loop, stop when a page
  returns fewer than `per_page` runs).
- [ ] **Step 3:** All four checks; commit —
  `feat: --auto queue count — one jobs call per queued run, label-filtered (design §5)`.

---

### Task 5: GitHub — runner naming and never-connected registration cleanup

**Files:**
- Modify: `src/github.rs`

**Interfaces:**
- Produces:

```rust
/// Fleet runner name: burst-<8 hex chars>. The nonce is the ownership
/// pattern the registration sweep keys on, so the format is a contract.
pub fn runner_name(nonce: &str) -> String {
    format!("burst-{nonce}")
}

/// Unique-per-mint nonce: 8 lowercase hex chars from time ^ pid ^ counter
/// (uniqueness within a process is what matters; the counter guarantees it).
pub fn runner_nonce() -> String;

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

/// Registrations safe to delete: offline, not busy, and burst-named. A JIT
/// runner that ran its job is deregistered by GitHub automatically — what
/// remains are never-connected mints (VM died before registering).
pub fn dead_registrations(rs: &[RunnerRegistration]) -> Vec<&RunnerRegistration> {
    rs.iter()
        .filter(|r| !r.online && !r.busy && is_burst_runner_name(&r.name))
        .collect()
}

impl Client {
    /// GET /repos/{o}/{r}/actions/runners?per_page=100, paginated.
    pub fn list_runners(&self, repo: &RepoId) -> Result<Vec<RunnerRegistration>, Error>;
    /// DELETE /repos/{o}/{r}/actions/runners/{id}. Re-verifies ownership at
    /// the destructive site: refuses (Error::GitHub, op "delete runner")
    /// unless is_burst_runner_name(name) — even though callers select via
    /// dead_registrations, the last check lives here. 404 is Ok (GitHub
    /// GC'd it first — the delete is idempotent).
    pub fn delete_runner(&self, repo: &RepoId, id: u64, name: &str) -> Result<(), Error>;
}
```

**Steps:**

- [ ] **Step 1:** Failing unit tests: `runner_name("deadbeef") == "burst-deadbeef"` and
  `is_burst_runner_name` accepts it; rejects `"burst-DEADBEEF"`, `"burst-123"`,
  `"my-home-runner"`, `"burst-deadbeef1"`; two `runner_nonce()` calls differ and both
  produce accepted names; `dead_registrations` keeps only offline+idle+burst-named out of
  a fixture containing an online burst runner (in-flight — kept), a busy one (kept), an
  offline burst one (selected), and an offline runner named `"home"` (kept: not ours).
- [ ] **Step 2:** Implement; `delete_runner`'s refusal branch is unit-testable without
  network (the name check precedes any HTTP) — test it refuses `name = "home"`.
- [ ] **Step 3:** All four checks; commit —
  `feat: runner naming contract + never-connected registration cleanup, ownership-checked delete`.

---

### Task 6: Extract the shared image-ensure path from `bake`

`up` needs everything `bake::run` does (token → agent version → connect → substrate → base
AMI → render → key → `Cloud::bake`), but interleaved with sweep and preflight. Factor once
so there is one authoring site for the sequence.

**Files:**
- Create: `src/commands/image.rs`; Modify: `src/commands/mod.rs`, `src/commands/bake.rs`

**Interfaces:**
- Produces:

```rust
// src/commands/image.rs

/// Everything up/bake share before any fleet decision: GitHub client +
/// resolved agent version (GitHub first — a PAT problem aborts before any
/// AWS resource exists), then the connected AwsCloud and the image-cache
/// key. Does NOT call Cloud::bake — callers decide when (bake: immediately;
/// up: after sweep-on-entry and the fork-approval preflight).
pub struct Prepared {
    pub client: crate::github::Client,
    pub cloud: crate::cloud::aws::AwsCloud,
    pub key: String,
}

pub fn prepare(config: &Config) -> Result<Prepared, Error>;
```

  Body is `bake::run` minus the final `bake(&key)` + print, moved verbatim (including the
  fail-loud unpinned-`base_ami` branch with its copy-pasteable remedy).
- `bake::run` becomes:

```rust
pub fn run(config: &Config) -> Result<(), Error> {
    let mut p = super::image::prepare(config)?;
    let image_id = p.cloud.bake(&p.key)?;
    println!("image ready: {image_id} ({})", p.key);
    Ok(())
}
```

**Steps:**

- [ ] **Step 1:** Pure refactor — no behavior change intended, so no new tests; the
  existing offline suite (CLI fail-loud tests, payload/key tests) is the regression net.
  Move the code, wire `pub mod image;`.
- [ ] **Step 2:** All four checks; commit —
  `refactor: extract commands::image::prepare — one authoring site for the pre-fleet sequence`.

---

### Task 7: `burst sweep` — expired instances, orphan schedules, dead registrations

**Files:**
- Create: `src/commands/sweep.rs`; Modify: `src/commands/mod.rs`, `src/main.rs`

**Interfaces:**
- Produces:

```rust
// src/commands/sweep.rs

/// What a sweep decided to do — a closed set, so the report and the
/// executor stay exhaustive together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepAction {
    /// Instance past its burst-actions-expires (or with a missing/garbled
    /// expiry — mirroring reconcile: never trusted, treated as expired).
    TerminateExpired { instance_id: String },
    /// Armed schedule whose instance is no longer live anywhere.
    DisarmOrphanSchedule { instance_id: String },
    /// Never-connected burst-named offline registration.
    DeleteDeadRegistration { id: u64, name: String },
}

/// Pure planner. `repo_instances` drives expiry (sweep is repo-scoped for
/// terminations, like every command); `all_live` spans ALL repos and exists
/// solely so another repo's live instance's schedule is never called an
/// orphan.
pub fn plan(
    now: DateTime<Utc>,
    repo_instances: &[Instance],
    all_live: &[Instance],
    armed: &[String],
    runners: &[crate::github::RunnerRegistration],
) -> Vec<SweepAction>;
```

  Expiry rule (single helper, shared shape with `reconcile`):
  `TAG_EXPIRES` parsed RFC3339; parse failure or absence ⇒ expired. Orphan rule: armed id
  with no `is_live()` instance of that id in `all_live`. Registrations: exactly
  `github::dead_registrations`.

```rust
/// Execute a plan. Terminates go through Cloud::terminate (which re-verifies
/// the burst-actions=1 tag immediately before acting); disarms through
/// Cloud::disarm_kill (idempotent); registration deletes through
/// Client::delete_runner (which re-verifies the name pattern). Idempotent by
/// construction: a second sweep plans nothing.
pub fn execute(
    cloud: &mut impl Cloud,
    client: &crate::github::Client,
    repo: &RepoId,
    actions: &[SweepAction],
) -> Result<(), Error>;

/// `burst sweep`: prepare (Task 6), list, plan, execute, print one line per
/// action + a summary ("sweep: nothing to do" when empty).
pub fn run(config: &Config) -> Result<(), Error>;

/// Shared entry for up's sweep-on-entry: same list/plan/execute over an
/// already-prepared cloud+client, so up pays no second connect.
pub fn sweep_with(
    cloud: &mut crate::cloud::aws::AwsCloud,
    client: &crate::github::Client,
    repo: &RepoId,
) -> Result<Vec<SweepAction>, Error>;
```

- `src/main.rs`: `Cmd::Sweep` arm calls `commands::sweep::run(&config)` (same error →
  stderr/exit-1 shape as `Bake`).

**Steps:**

- [ ] **Step 1:** Failing unit tests on `plan` (fixtures via `FakeCloud`-style
  `Instance`s):
  - expired instance selected; unexpired not; missing-`burst-actions-expires` tag selected;
    garbled date selected (the reconcile precedent, re-pinned here);
  - armed kill for a live instance in `all_live` but **not** in `repo_instances` (another
    repo's) → NOT an orphan; armed kill for an id in neither → orphan;
  - runner fixtures flow through unchanged from Task 5's rules;
  - empty inputs → empty plan.
  And on `execute` against `FakeCloud`: a planned terminate+disarm leaves
  `list_tagged` without the instance and `armed_kills()` without the schedule; running
  `plan` again over the post-execute state returns empty (**idempotence pinned as a
  test**).
- [ ] **Step 2:** Implement; wire `main.rs`; update the CLI integration test
  `subcommands_fail_loud_not_silent` to drop `"sweep"` from the not-implemented list
  (offline it now fails loud with a GitHub-token or AWS error, still exit 1 — assert
  that, as was done for `bake`).
- [ ] **Step 3:** All four checks; commit —
  `feat: burst sweep — expired instances, cross-repo-safe orphan schedules, dead registrations; idempotent`.

---

### Task 8: `burst status` — cloud-truth text

**Files:**
- Create: `src/commands/status.rs`; Modify: `src/commands/mod.rs`, `src/main.rs`

**Interfaces:**
- Produces:

```rust
// src/commands/status.rs

/// Render the fleet as text. Pure — the one authoring site for status
/// wording (humans, scripts, and agents in CI logs all read it; one
/// spelling per condition).
pub fn render(
    repo: &RepoId,
    now: DateTime<Utc>,
    instances: &[Instance],
    armed: &[String],
    statefile_present: bool,
) -> String;
```

  Output shape (exact strings pinned by test):

```
fleet for octo/widgets: 2 live
  i-0aaa  running  expires 2026-08-09T18:00:00Z (in 5h58m)  kill-armed
  i-0bbb  pending  expires 2026-08-09T18:00:00Z (in 5h58m)  kill-armed
statefile: present (a watcher was attached from this host)
```

  Zero fleet: `fleet for octo/widgets: none`. An instance whose id is missing from
  `armed` prints `KILL SCHEDULE MISSING` instead of `kill-armed` — that is a layer-1 gap
  worth shouting about, not a formatting detail. Expired prints `EXPIRED` in place of the
  countdown. States render via an exhaustive `match` on `InstanceState` (no `_`).
- `run(config: &Config) -> Result<(), Error>`: cloud truth only — `AwsContext::connect` +
  `ensure_substrate` are **not** needed; connect, then `list_tagged` + `list_armed_kills`
  require an `AwsCloud`… which requires a `Substrate`. Keep honest: `status` calls
  `image::prepare`? No — `prepare` hits GitHub and may fail on an unpinned AMI, wrong for
  a read-only command. Instead `run` builds the minimal read path:
  `AwsContext::connect(region)` → `ensure_substrate(None)` (idempotent get-or-create; on
  any account that has ever run burst this is pure gets) → an `AwsCloud` with empty
  bake-only fields (`base_ami`/`provisioning_script` empty strings, never used by
  `list_*`) → `render` → print. Reads statefile presence via
  `state::RepoState::open(&config.repo)` + `read()?.is_some()` (no lock taken — status
  must work while a watcher runs).
- `src/main.rs`: `Cmd::Status` arm wired like the others.

**Steps:**

- [ ] **Step 1:** Failing unit tests on `render`: the exact lines above from fixture
  instances (one running kill-armed, one pending); `none` case; `EXPIRED` case;
  `KILL SCHEDULE MISSING` case; statefile absent line reads
  `statefile: none (no watcher from this host)`.
- [ ] **Step 2:** Implement + wire `main.rs`; update `subcommands_fail_loud_not_silent`
  for `"status"` as in Task 7.
- [ ] **Step 3:** All four checks; commit —
  `feat: burst status — cloud-truth text, shouts a missing kill schedule`.

---

### Task 9: `burst down` — tag-verified teardown

**Files:**
- Create: `src/commands/down.rs`; Modify: `src/commands/mod.rs`, `src/main.rs`

**Interfaces:**
- Produces:

```rust
// src/commands/down.rs

/// y/N confirmation, injected reader so tests never touch stdin. --yes
/// bypasses. Anything but "y"/"yes" (case-insensitive, trimmed) is No.
pub fn confirm(prompt: &str, yes_flag: bool, input: &mut impl std::io::BufRead) -> bool;

pub fn run(config: &Config, yes: bool) -> Result<(), Error>;
```

  `run` sequence:
  1. Connect + substrate + `AwsCloud` (Task 8's minimal read path — no GitHub needed to
     *list*), `list_tagged(&config.repo)`.
  2. Empty → `println!("no live fleet for {repo} — nothing to terminate")`, Ok. (An
     accurate answer, not a degradation.)
  3. `confirm("terminate N instances for {repo}? [y/N] ", yes, stdin)`; No → print
     `"aborted"` and Ok.
  4. `cloud.terminate(&ids)` — ownership re-verified inside, all-or-nothing.
  5. `cloud.disarm_kill(id)` per instance (idempotent; their schedules are now pointless).
  6. GitHub tidy: `token_from_env` + `list_runners` + delete `dead_registrations` (a VM
     killed mid-job is deregistered by GitHub when the runner vanishes; only
     never-connected mints remain). A GitHub failure here is a **warning**, not a rollback
     — the instances are already down, which is the billing-relevant fact; print the
     error, still exit Ok, registrations are cosmetic (design §3) and the next sweep
     retries.
  7. Delete the statefile via `RepoState::open(&config.repo)` (take the lock first with
     `lock()`; if `LockHeld`, a watcher is attached from this host — leave the statefile,
     print that the watcher will notice the fleet is gone and tidy up itself).
- `src/main.rs`: `Cmd::Down { yes }` wired.

**Steps:**

- [ ] **Step 1:** Failing unit tests: `confirm` — `yes_flag` short-circuits without
  reading; `"y\n"`, `"YES\n"` → true; `"\n"`, `"n\n"`, `"anything\n"` → false. A
  `FakeCloud`-driven test of the teardown core (factor steps 4–5 as
  `fn teardown(cloud: &mut impl Cloud, ids: &[String]) -> Result<(), Error>`): after
  teardown, `list_tagged` is empty and `armed_kills()` is empty.
- [ ] **Step 2:** Implement + wire `main.rs`; update `subcommands_fail_loud_not_silent`
  for `"down"`.
- [ ] **Step 3:** All four checks; commit —
  `feat: burst down — confirm, tag-verified terminate, disarm, registration tidy, statefile removal`.

---

### Task 10: Fleet sizing — `--auto`, `max_fleet`, quota check

**Files:**
- Create: `src/commands/up.rs` (sizing half); Modify: `src/commands/mod.rs`,
  `src/cloud/aws.rs`, `docs/permissions.md` (flagged scope growth)

**Interfaces:**
- Produces (pure, in `src/commands/up.rs`):

```rust
/// Requested fleet size before quota: explicit N, or the --auto count,
/// capped at max_fleet. Zero is a valid answer (up prints "no queued burst
/// jobs — nothing to launch" and exits 0 — accurate, not degraded).
pub fn fleet_size(explicit_n: Option<u32>, auto_count: Option<u32>, max_fleet: u32) -> u32 {
    let n = explicit_n.or(auto_count).unwrap_or(0);
    n.min(max_fleet)
}

/// Decision 9: warn BEFORE capping, never half-launch silently. Returns the
/// launchable count and, when capped, the one warning message (single
/// authoring site).
pub fn quota_cap(
    requested: u32,
    vcpus_per_instance: u32,
    headroom_vcpus: u32,
) -> (u32, Option<String>) {
    let fits = if vcpus_per_instance == 0 { requested } else { headroom_vcpus / vcpus_per_instance };
    if fits >= requested {
        (requested, None)
    } else {
        (
            fits,
            Some(format!(
                "warning: vCPU quota caps the fleet — requested {requested} instances \
                 ({} vCPUs) but only {headroom_vcpus} vCPUs of quota headroom remain; \
                 launching {fits}. Leftover jobs fall to the home runner or a second \
                 `burst up` (request a quota increase in the AWS console to raise the cap)",
                requested * vcpus_per_instance
            )),
        )
    }
}
```

- Produces (live, on `AwsContext` in `src/cloud/aws.rs`):

```rust
/// vCPU headroom under the account's on-demand (L-1216C47A, "Running
/// On-Demand Standard instances") or spot (L-34B43A08, "All Standard Spot
/// Instance Requests") vCPU quota: quota value minus the vCPUs of currently
/// running instances (DescribeInstances, running, summed CpuOptions
/// core_count * threads_per_core). Quota codes are cached external facts —
/// verified at the gate.
pub fn vcpu_headroom(&self, spot: bool) -> Result<u32, Error>;

/// vCPUs of one instance of `instance_type`, via DescribeInstanceTypes.
pub fn vcpus_of(&self, instance_type: &str) -> Result<u32, Error>;
```

  A quota-probe failure (e.g. AccessDenied on `servicequotas:GetServiceQuota`) is a hard
  `Error::Aws` naming the missing permission — fail loud, never silently skip an advisory
  check the design promises (decision 9).
- `docs/permissions.md` gains `servicequotas:GetServiceQuota` +
  `ec2:DescribeInstanceTypes` in the read-only section, in a commit whose message flags it:
  this is the one deliberate permission addition of the phase and is **reported to James
  as scope growth**, per that doc's preamble.

**Steps:**

- [ ] **Step 1:** Failing unit tests: `fleet_size` — explicit wins, auto capped at
  `max_fleet`, zero flows through; `quota_cap` — no warning when it fits exactly;
  capped case returns the smaller count and a message containing `"warning"`, both
  numbers, and the quota-increase remedy; `vcpus_per_instance == 0` never divides by
  zero.
- [ ] **Step 2:** Implement the two `AwsContext` methods (live-verified at gate);
  `docs/permissions.md` edit in its own commit —
  `docs/permissions: +servicequotas:GetServiceQuota, +ec2:DescribeInstanceTypes for the decision-9 quota check (scope growth — flagged for James)`.
- [ ] **Step 3:** All four checks; commit —
  `feat: fleet sizing — --auto/max_fleet/quota-cap with warn-before-cap (decision 9)`.

---

### Task 11: `burst up` — the full loop

**Files:**
- Modify: `src/commands/up.rs`, `src/main.rs`, `src/commands/mod.rs`

**Interfaces:**
- Produces:

```rust
// src/commands/up.rs

pub struct UpArgs {
    pub n: Option<u32>,
    pub auto: bool,
    pub spot: bool,
    pub yes: bool,
    pub ssh_key: Option<String>,
}

pub fn run(config: &Config, args: &UpArgs) -> Result<(), Error>;
```

  `run` sequence — design §3's lifecycle, line for line:
  1. **Lock/adopt**: `RepoState::open(&config.repo)`; `lock()` (`LockHeld` → fail fast,
     the existing error). `read()?` → residue statefile or `None`.
  2. **Prepare** (`image::prepare`, Task 6): GitHub client + agent version first (a PAT
     problem or GitHub outage aborts before any AWS resource exists — design §3's
     "GitHub API down at launch" row), then connect + `ensure_substrate` + image key.
  3. **Reconcile + cross-host advisory**: `cloud.list_tagged(&config.repo)`;
     `reconcile::reconcile(&statefile_or_empty, &cloud_instances)`. If `adopted` is
     non-empty (live tagged instances this host's statefile doesn't know — including the
     no-statefile case), prompt via Task 9's `confirm`:
     `"someone else appears to be running burst workers for {repo} from another host ({k} unrecognized live instances) — continue? [y/N] "`
     (`args.yes` bypasses). Declined → print `"aborted"`, Ok. Adopted instances join the
     watch manifest either way once confirmed (tags stay authoritative); `dropped` ids
     are pruned. Print one line per adopted/dropped id.
  4. **Sweep-on-entry**: `sweep::sweep_with(&mut cloud, &client, &config.repo)` — rent
     paid on entry; expired residue (including just-adopted expired instances) dies here.
  5. **Preflight** (invariant 5): `client.fork_approval_policy(&config.repo)?` →
     `preflight_fork_approval(...)?`. Hard error — nothing has launched yet.
  6. **Size**: `auto_count = args.auto.then(|| client.queued_burst_job_count(&config.repo)).transpose()?`;
     `fleet_size(args.n, auto_count, config.max_fleet)`. Zero AND no adopted fleet →
     `"no queued burst jobs — nothing to launch"`, Ok. Zero with an adopted fleet →
     skip to watch (resume-only invocation).
  7. **Quota**: `vcpus_of(&config.instance_type)`, `vcpu_headroom(args.spot)`,
     `quota_cap(...)`; print the warning if `Some`, launch the capped count.
  8. **AMI ensure**: `cloud.bake(&p.key)?` — cache hit is the common ~zero-cost case;
     miss bakes inline (10–20 min) exactly as `burst bake` would.
  9. **Mint & launch & arm & record**, one VM at a time (each VM needs its own
     single-use JIT config, so `RunInstances` is 1×N, never N×1):

```rust
fn launch_fleet(
    cloud: &mut impl Cloud,
    state: &RepoState,
    manifest: &mut StateFile,
    count: u32,
    expires: DateTime<Utc>,
    spot: bool,
    ssh_key: Option<&str>,
    image_id: &str,
    instance_type: &str,
    repo: &RepoId,
    mint: &mut dyn FnMut(&str) -> Result<String, Error>, // runner name -> JIT blob
) -> Result<(), Error>
```

     Per iteration: `nonce = github::runner_nonce()`; `name = runner_name(&nonce)`;
     `jit = mint(&name)?`; `user_data = payload::fleet_user_data(&jit)?`;
     `cloud.launch(&LaunchSpec { count: 1, image_id, instance_type, spot,
     tags: TagSpec { repo, expires }, user_data, ssh_key })?`;
     `cloud.arm_kill(&id, expires)?`; push an `InstanceRecord { id, launched_at: now,
     expires_at: expires }` into `manifest.instances` and `state.write(manifest)?`
     **immediately** — the statefile trails reality by at most one instance, so a SIGKILL
     between any two calls leaves only tag-discoverable, kill-armed residue (invariant 2
     holds from `RunInstances`'s atomic `TagSpecifications`; layer 1 holds from the
     arm-immediately ordering). `expires = now + config.ttl_hours`.
     Any error mid-fleet wraps as
     `Error::PartialLaunch { launched, requested, message }` (Task 1) — loud, truthful,
     and the already-launched fleet is recorded + kill-armed; exit 1, no watch (the
     invariant-3 layers own cleanup; re-run to re-attach, as the message says).
     Edge: `launch` succeeded but `arm_kill` failed → terminate that one instance
     (tag-verified) before erroring, mirroring bake's arm-failure handling — an unfenced
     instance never survives an error path.
  10. **Watch** (observer only — invariant 3):

```rust
/// Poll until every watched instance is gone. Returns Detached if Ctrl-C
/// was received (closed set — the caller matches exhaustively).
#[derive(Debug, PartialEq, Eq)]
pub enum WatchOutcome {
    FleetGone,
    Detached { live: usize },
}

fn watch(
    cloud: &mut impl Cloud,
    repo: &RepoId,
    detach: &std::sync::atomic::AtomicBool,
    poll: Duration, // 30 s in run(); tests pass Duration::ZERO
) -> Result<WatchOutcome, Error>
```

     Loop: `list_tagged` → print `"fleet: {n} live"` (only when the count changes — CI
     logs are read by agents; don't spam); `n == 0` → `FleetGone`; `detach` set →
     `Detached`. `ctrlc::set_handler` in `run()` sets the flag; on `Detached` print
     `"detaching — fleet still running ({live} instances); it will finish and self-terminate. Re-run `burst up` to re-attach, `burst down` to tear down"`
     and return Ok **without** deleting the statefile (it is the adoptable-residue signal).
  11. **Final tidy** (on `FleetGone` only): `disarm_kill` each manifest id (their
     one-shots are now pointless; idempotent if already fired), delete
     `dead_registrations` via the client (warning-not-error on GitHub failure, as in
     `down`), `state.delete()?`, print `"fleet drained — all clean"`.
- `src/main.rs`: `Cmd::Up { n, auto, spot, yes, ssh_key }` → `commands::up::run(&config,
  &UpArgs { .. })`.

**Steps:**

- [ ] **Step 1:** Failing unit tests against `FakeCloud` (+ tempdir `RepoState`):
  - `launch_fleet` launches N with distinct user-data blobs (mint closure returns
    `format!("{name}base64")`-shaped distinct blobs — assert each launched instance's
    existence and `armed_kills().len() == N`), and the statefile on disk holds N records;
  - `launch_fleet` with a mint closure that fails on the 3rd call → `PartialLaunch`
    naming 2 of 3, statefile holds exactly 2 records, `armed_kills().len() == 2`;
  - arm-failure edge: a `FakeCloud` wrapper whose `arm_kill` errors → the launched
    instance is terminated (absent from `list_tagged`) and the error is loud;
  - `watch` with `Duration::ZERO`: fleet planted then terminated between polls (drive
    via a wrapper or terminate before the second poll) → `FleetGone`; flag pre-set →
    `Detached` on the first check without waiting;
  - `fleet_size`/`quota_cap` already covered (Task 10).
- [ ] **Step 2:** Implement `run` + wire `main.rs`; update
  `subcommands_fail_loud_not_silent` for `"up"` (offline: loud GitHub-token/AWS error,
  exit 1).
- [ ] **Step 3:** **Regress-to-prove**: temporarily invert `preflight_fork_approval`'s
  `AllExternalContributors` arm to `Err(..)` — the Task 3 unit tests must go red (guard
  teeth proven); revert. Record in the commit message that this was done.
- [ ] **Step 4:** All four checks; commit —
  `feat: burst up — full §3 lifecycle: adopt, sweep, preflight, ensure, mint/launch/arm/record, observer watch, detach`.

---

### Task 12: Gate — the kill-test matrix (lead-run, live, watched; NOT plan code)

**Files:** none created; findings recorded in the session report. Everything below is
executed manually by the lead on the real account against a **synthetic queued matrix**:
a throwaway workflow in the target repo with `strategy: matrix` of ~4 sleep-jobs on
`runs-on: [self-hosted, burst]` (e.g. `run: sleep 120`). Gate `burst.toml`:
`instance_type = "t3.micro"`, real repo, pinned Debian AMI. **Every instance carries the
tag triple + armed schedule from launch by construction; verify, don't assume.**
Distinguish "I ran it and saw X" from inference for every row.

The sweep-equivalent listing that ends **every** session:

```
aws ec2 describe-instances \
  --filters Name=tag:burst-actions,Values=1 \
            Name=instance-state-name,Values=pending,running,shutting-down,stopping,stopped \
  --query 'Reservations[].Instances[].[InstanceId,State.Name,Tags]' --output table
aws scheduler list-schedules --name-prefix burst-actions- --query 'Schedules[].Name'
```

Expected at session end: empty table, empty schedule list — stated in the report.

- [ ] **G1 — happy path first** (baseline before any kill): queue the sleep matrix,
  `burst up --auto` → observe: correct count printed, fleet launches, runners appear in
  Settings→Runners, jobs drain, VMs self-terminate, watcher prints `fleet drained — all
  clean`, statefile gone, schedules gone (disarmed), zero residue by the listing above.
- [ ] **G2 — CLI SIGKILLed mid-launch (layers 2/3/4)**: queue the matrix, `burst up 3`,
  `kill -9` the CLI after the first `fleet:` progress line. Watch: the fleet finishes its
  jobs and self-terminates with no CLI alive (layer 3 poweroff = layer 2 terminate).
  Then run `burst up 0`-equivalent (`burst up --auto` with an empty queue) and watch it
  **adopt the residue**: statefile reconciled, dropped ids printed, sweep disarms the
  now-orphan schedules (layer 4). Record what each invocation printed.
- [ ] **G3 — broken AMI boot → bootstrap deadline through the full `up` path (layer
  3)**: temporarily point `base_ami`… no — the AMI must be *broken*, not the base: bake a
  deliberately broken image by hand-editing the provisioning template so
  `burst-runner.service` can't start (e.g. wrong runner path), `burst bake`, then
  `burst up 1` with an empty queue. Watch the instance terminate at ~10 min via the
  bootstrap deadline while the watcher merely reports the shrinking fleet. Restore the
  template, rebake (supersession GC observed again for free).
- [ ] **G4 — hung job → on-VM TTL, and wedged VM → EventBridge alone (layers 3 then
  1)**: (a) set `ttl_hours = 1` in gate config, queue one `sleep 7200` job, `burst up 1`;
  watch the on-VM TTL timer poweroff at the 1 h mark and GitHub mark the job failed.
  (b) Layer 1 **demonstrably alone**: launch one VM through the tool, then from an SSH
  debug session (`--ssh-key`) `systemctl disable --now` all three burst timers and hang
  the box (`systemctl stop actions-runner`-equivalent; leave it idle); watch the
  `burst-actions-i-*` schedule fire `TerminateInstances` at TTL and **self-delete**
  (schedule disappears from `list-schedules` — `ActionAfterCompletion=DELETE` observed).
  Also re-verify Task 2's fix here: a bake timeout terminates the builder exactly once
  (CloudTrail or console events show one TerminateInstances).
- [ ] **G5 — cancelled workflow → idle timeout (layer 3)**: queue the matrix,
  `burst up --auto`, cancel the workflow run in the UI once VMs are registered. Watch:
  runners go jobless, never-assigned idle timeout powers each off at ~10 min, watcher
  reports the fleet draining to zero.
- [ ] **G6 — fork-approval preflight hard-errors**: weaken the repo setting (Settings →
  Actions → General → set fork-PR approval to "first-time contributors"), `burst up 1` →
  hard error **before any AWS mutation** (verify: no new instance, no new schedule);
  error text names the setting and the fix; follow the printed remedy and confirm the
  path is real. Restore the setting; `burst up` proceeds. This is also the live
  verification of Task 3's endpoint/policy-string external facts — record the actual
  JSON seen.
- [ ] **G7 — sweep reaps artificial expiry + orphan schedule (layer 4)**: launch one VM
  through the tool, then retag it by hand:
  `aws ec2 create-tags --resources <id> --tags Key=burst-actions-expires,Value=2020-01-01T00:00:00Z`.
  Also create one orphan schedule by hand under the exact name shape:
  `burst-actions-i-0deadbeef` (nonexistent instance). `burst sweep` → the instance is
  terminated (tag-verified), the orphan schedule deleted, the VM's own schedule
  disarmed; a second `burst sweep` prints `sweep: nothing to do` (idempotence, live).
- [ ] **G8 — concurrency + cross-host advisory**: (a) during a live `burst up` watch,
  a second `burst up 1` in another terminal fails fast with the lock-held error.
  (b) Simulate another host: with a fleet live, move the statefile aside
  (`mv ~/.local/state/burst/<owner>-<repo>/state.json /tmp/`), run `burst up 1` → the
  advisory prompt appears naming the unrecognized instance count; decline → aborted,
  nothing launched; re-run with `--yes` → proceeds and **adopts** (statefile restored
  contains the adopted ids). Restore state.
- [ ] **G9 — --auto count correctness**: with a queued matrix of exactly 4 burst jobs +
  1 queued `home`-labeled job, `burst up --auto` prints/launches 4, not 5 (the
  label filter observed against real API responses; record the per-run call count
  matches the number of queued runs).
- [ ] **G10 — quota + status**: `burst status` during a live fleet shows every instance
  `kill-armed` with sane expiries; `vcpu_headroom` returns a plausible number (compare
  against the console's quota page — the L-code external facts verified); if
  quota-capping can be provoked cheaply (request > headroom/vcpus), watch the warning
  print before the capped launch.
- [ ] **G11 — zero-residue close**: sweep-equivalent listing empty; GitHub
  Settings→Runners shows no burst-named registrations; full offline suite + all four
  checks green on the final tree. State all three in the report.

---

## Phase gate checklist (from implementation-phases.md, watched)

- [ ] CLI SIGKILLed mid-launch → fleet finishes and self-terminates; next invocation
  adopts residue (layers 2/3/4) — G2.
- [ ] Broken AMI boot → bootstrap-deadline poweroff through the full `up` path (layer 3)
  — G3.
- [ ] Hung job → on-VM TTL cap; wedged VM with on-VM timers disabled → EventBridge kill
  acting demonstrably alone (layers 3 then 1) — G4.
- [ ] Cancelled workflow → jobless runner → idle-timeout poweroff (layer 3) — G5.
- [ ] `sweep` reaps an artificially expired instance and its orphan schedule (layer 4),
  idempotently — G7.
- [ ] Fork-approval preflight: weakened repo setting → `burst up` hard-errors before any
  AWS mutation — G6.
- [ ] Concurrency: second `up` fails fast; hidden-statefile "another host" triggers the
  advisory prompt; `--yes` bypasses and adopts — G8.
- [ ] Zero live instances, zero schedules, zero burst registrations at session end — G11.
