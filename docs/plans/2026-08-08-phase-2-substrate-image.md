# Phase 2: Substrate & Image — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** First real AWS contact: the idempotent `ensure_substrate()`, the AWS `Cloud` backend
(`launch` / `terminate` / `list_tagged` / `arm_kill`), the VM payload in `vm/`, `burst bake`
end-to-end with the one-generation image cache, and the thin GitHub slice (PAT auth + JIT
minting + agent-version resolution) needed to measure boot-to-registered < 2 min at the gate.

**Architecture:** The `Cloud` trait stays **sync** — `AwsCloud` owns a
`tokio::runtime::Runtime` and `block_on`s each `aws-sdk-rust` call internally. (Phase 1's
trait comment said "phase 2 makes these async"; keeping the trait sync instead is the smaller
change — no churn in `FakeCloud`, `reconcile`, or their tests, and no async colouring of the
command layer for a CLI that does nothing concurrently yet. Internal seam, reversible in phase
3 if the watcher wants concurrency. Recorded here as a deliberate deviation, not drift.)
All AWS calls stay inside the IAM policy in `docs/permissions.md` — a call not covered there
is a design error, not a policy edit. Pure logic (retry classification, schedule naming,
user-data/provision rendering, cache-key lookup semantics) is unit-tested offline; live-AWS
behavior is verified manually by the lead at the gate, never in `cargo test`.

**Tech Stack:** existing phase-1 stack plus: `tokio` (rt only), `aws-config`, `aws-sdk-ec2`,
`aws-sdk-scheduler`, `aws-sdk-iam`, `aws-sdk-budgets`, `ureq` 3 + `serde_json` for the GitHub
REST slice (sync, matching the trait), `base64` (user-data encoding).

## Global Constraints

- **Never weaken invariants 3/4/5**: no cleanup path may depend on the watcher/CLI being
  alive; the PAT never reaches a VM in any form — a VM gets exactly one single-use JIT config;
  the fork-approval preflight (phase 3) is not pre-weakened by anything built here.
- **Real money discipline**: every instance created — including by-hand gate experiments —
  carries the tag triple *and* an armed kill schedule from launch. Gate testing uses the
  smallest viable type (`t3.micro`); every working session ends with a sweep-equivalent listing
  (command given in the Gate section) showing zero live instances, reported as done.
- Tags exactly (constants already in `src/schema.rs`): `burst-actions=1`,
  `burst-actions-repo=<owner/repo>`, `burst-actions-expires=<ISO8601>`. AWS resource names
  `burst-actions-*`. One new tag key this phase: `burst-actions-image-key`.
- **Prove ownership before destroy**: every terminate/deregister/delete re-verifies the
  `burst-actions=1` tag on the specific resource immediately before acting, even when a tag
  filter already selected it.
- `RunInstances` always: atomic `TagSpecifications` (instance **and** volume),
  `instance_initiated_shutdown_behavior` set explicitly (fleet: `terminate`; builder: `stop`),
  `HttpTokens=required` (IMDSv2), the zero-inbound `burst-actions` SG, no public-inbound rule
  ever created.
- GitHub PAT read from `BURST_GITHUB_TOKEN`, falling back to `GITHUB_TOKEN`; missing both is a
  loud error naming both variables.
- Fail loud, never degrade: missing region, missing default VPC, missing token, bake timeout —
  each names its remedy; verify each remedy works before shipping the message.
- Closed sets are exhaustively-matched enums, never strings; no `_ =>` arm on our own enums.
  New AWS-visible states/outcomes get enum variants so the compiler finds every match site.
- All four checks before **every** commit:
  `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`.
- Commit as you go: one verified change per commit, honest messages, trailer lines copied from
  `git log -1`.
- §8 defaults as written: idle 10 min, TTL 6 h, pinned Ubuntu LTS base (24.04, x86_64),
  fail-loud on missing default VPC.
- `cargo test` must pass with **no** AWS credentials and **no** network: nothing offline may
  construct a live client eagerly or read AWS env at test time.

---

### Task 1: Dependencies, state-enum extension, error taxonomy

**Files:**
- Modify: `Cargo.toml`, `src/cloud/mod.rs`, `src/error.rs`, `src/reconcile.rs` (compiler-forced)

**Interfaces:**
- Consumes (unchanged, quoted from `src/cloud/mod.rs`):

```rust
pub trait Cloud {
    /// Launch spec.count instances with spec.tags applied atomically at creation.
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error>;
    fn terminate(&mut self, ids: &[String]) -> Result<(), Error>;
    /// All non-terminated instances carrying burst-actions=1 and burst-actions-repo=<repo>.
    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error>;
    /// Arm the control-plane one-shot kill for one instance at `at`.
    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error>;
    /// Get-or-create the image for `key`; returns the image id.
    fn bake(&mut self, key: &str) -> Result<String, Error>;
}
```

- Produces: `InstanceState` gains `Stopping` and `Stopped` (the builder stops before
  `CreateImage`; EC2 can report both). Both are **live** in `is_live()` — a stopped instance
  still exists and bills EBS, so it must be adopted/swept, never silently dropped.
- New `error::Error` variants (one authoring site each):

```rust
    #[error("no GitHub token: set BURST_GITHUB_TOKEN (or GITHUB_TOKEN) to a fine-grained PAT with Administration read/write on the target repo")]
    GitHubTokenMissing,
    #[error("GitHub API {op} failed ({status}): {message}")]
    GitHub { op: &'static str, status: u16, message: String },
    #[error("no AWS region configured: set AWS_REGION, add region to your AWS profile, or set region in burst.toml")]
    RegionMissing,
    #[error("AWS {op} failed: {message}")]
    Aws { op: &'static str, message: String },
    #[error("no default VPC in {region}: burst launches into the default VPC only — create one with `aws ec2 create-default-vpc --region {region}`")]
    NoDefaultVpc { region: String },
    #[error("bake timed out: builder {instance_id} did not reach 'stopped' within {minutes} min — provisioning likely failed; the builder was terminated and its kill schedule deleted")]
    BakeTimeout { instance_id: String, minutes: u64 },
```

**Steps:**

- [ ] **Step 1:** Add to `Cargo.toml` `[dependencies]`:

```toml
tokio = { version = "1", features = ["rt"] }
aws-config = { version = "1", features = ["behavior-version-latest"] }
aws-sdk-ec2 = "1"
aws-sdk-scheduler = "1"
aws-sdk-iam = "1"
aws-sdk-budgets = "1"
ureq = { version = "3", features = ["json"] }
base64 = "0.22"
```

  Run `cargo build` — clean.
- [ ] **Step 2:** Add the two `InstanceState` variants; `cargo build` fails at every
  non-exhaustive match (that's the point). Fix `is_live` (`Stopping | Stopped => true`) and any
  fake/reconcile match the compiler names. Add a unit test in `src/cloud/mod.rs`:
  `assert!(InstanceState::Stopped.is_live())` with a comment: stopped bills EBS, must be swept
  not forgotten.
- [ ] **Step 3:** Add the `Error` variants above. `cargo test` — all phase-1 tests still green.
- [ ] **Step 4:** All four checks; commit —
  `feat: phase-2 deps; InstanceState Stopping/Stopped (live); AWS/GitHub error variants`.

---

### Task 2: GitHub slice — token, agent version, JIT mint

**Files:**
- Create: `src/github.rs`, `examples/mint_jit.rs`; Modify: `src/lib.rs` (`pub mod github;`)

**Interfaces:**
- Consumes: `schema::RepoId` (`pub fn owner(&self) -> &str`, `pub fn name(&self) -> &str`),
  `error::Error`.
- Produces:
  - `github::token_from_env() -> Result<Token, Error>` — `BURST_GITHUB_TOKEN` else
    `GITHUB_TOKEN` else `Error::GitHubTokenMissing`. `Token` is a newtype whose `Debug` prints
    `Token(***)` — the secret never reaches logs.
  - `github::Client::new(token: Token) -> Client` (base URL parameterizable for tests:
    `Client::with_base_url(token, url)`).
  - `Client::runner_agent_version(&self) -> Result<String, Error>` —
    `GET /repos/actions/runner/releases/latest`, returns `tag_name` with the leading `v`
    stripped (e.g. `2.328.0`). This string is the image-key input `runner_agent_version`.
  - `Client::mint_jit_config(&self, repo: &RepoId, runner_name: &str) -> Result<String, Error>`
    — `POST /repos/{owner}/{repo}/actions/runners/generate-jitconfig` with body
    `{"name": runner_name, "runner_group_id": 1, "labels": ["self-hosted","burst"], "work_folder": "_work"}`;
    returns the response's `encoded_jit_config`. Exactly one mint per VM; the caller never
    reuses a blob.
  - Pure, unit-tested: `github::jit_request_body(runner_name: &str) -> serde_json::Value` and
    `github::strip_release_tag(tag: &str) -> String`.

**Steps:**

- [ ] **Step 1:** Failing unit tests in `src/github.rs`: body has exactly the four keys with
  labels `["self-hosted","burst"]`; `strip_release_tag("v2.328.0") == "2.328.0"` and idempotent
  on `"2.328.0"`; `format!("{:?}", Token::from("x"))` contains no `x`; `token_from_env`
  precedence tested via an injected-lookup helper
  `token_from(vars: impl Fn(&str) -> Option<String>)` (no global env mutation in tests).
- [ ] **Step 2:** Implement with `ureq`: headers `Authorization: Bearer <token>`,
  `X-GitHub-Api-Version: 2022-11-28`, `User-Agent: burst`. Non-2xx → `Error::GitHub` carrying
  the response body's `message` field when parseable, raw text otherwise. 401/403 messages must
  read as "token invalid or expired — rotate it" (design decision 8).
- [ ] **Step 3:** `examples/mint_jit.rs` — gate tooling, not shipped in the binary:

```rust
// cargo run --example mint_jit -- owner/repo runner-name
// Prints the encoded JIT config to stdout (and nothing else) for manual gate launches.
fn main() {
    let mut args = std::env::args().skip(1);
    let repo = burst::schema::RepoId::parse(&args.next().expect("owner/repo")).unwrap();
    let name = args.next().expect("runner-name");
    let client = burst::github::Client::new(burst::github::token_from_env().unwrap());
    println!("{}", client.mint_jit_config(&repo, &name).unwrap());
}
```

- [ ] **Step 4:** All four checks (no live call in tests); commit —
  `feat: github slice — token env chain, agent-version resolve, single-use JIT mint`.
- [ ] **Step 5 (lead, live, flagged as such in the report):**
  `cargo run --example mint_jit -- <real-repo> gate-probe` returns a blob; the runner appears
  under repo Settings → Actions → Runners as offline/never-connected. Record "I ran it and saw
  X". (GitHub GCs never-connected JIT registrations; phase-3 sweep also deletes them.)

---

### Task 3: AWS context probe — credentials, region, default VPC

**Files:**
- Create: `src/cloud/aws.rs` (module skeleton + context); Modify: `src/cloud/mod.rs`
  (`pub mod aws;`)

**Interfaces:**
- Consumes: `config::Config { pub region: Option<String>, .. }`, `error::Error`.
- Produces:
  - `aws::AwsContext` — `pub fn connect(region_override: Option<&str>) -> Result<AwsContext, Error>`:
    builds a single-thread `tokio::runtime::Runtime`, loads
    `aws_config::defaults(BehaviorVersion::latest())` with `region_override`
    (config `region` wins over the profile/env chain); **no region resolvable →
    `Error::RegionMissing`** (the error text's three remedies are exactly: `AWS_REGION`,
    profile region, `burst.toml` region — verify each works at the gate). Holds the runtime,
    `SdkConfig`, and constructed `ec2`/`scheduler`/`iam`/`budgets` clients.
  - `AwsContext::default_vpc_and_subnet(&self) -> Result<(String, String), Error>` —
    `DescribeVpcs` filter `is-default=true`; none → `Error::NoDefaultVpc { region }`; then
    `DescribeSubnets` filter `vpc-id`, `default-for-az=true`, take the first (deterministic:
    sort by AZ name). Both calls are in the policy's `ReadOnlyDescribes`.
  - Credential probe is implicit: the first `Describe*` call failing with a credentials error
    maps to `Error::Aws { op, message }` whose message names the fix ("configure AWS
    credentials: env vars or `aws configure`"). No STS dependency, no extra permission.

**Steps:**

- [ ] **Step 1:** Unit test the pure part only: region-precedence helper
  `fn effective_region(config_region: Option<&str>, chain_region: Option<&str>) -> Result<String, Error>`
  — config wins, chain fallback, neither → `RegionMissing`. Client construction is not
  unit-tested (live behavior belongs to the gate).
- [ ] **Step 2:** Implement; ensure nothing in this module runs at `cargo test` time without
  being explicitly called.
- [ ] **Step 3:** All four checks; commit — `feat: AwsContext — region chain (config wins, fail-loud), default-VPC/subnet probe`.

---

### Task 4: `ensure_substrate()` — idempotent get-or-create

**Files:**
- Modify: `src/cloud/aws.rs`; Modify: `src/config.rs` + `src/error.rs` (one opt-in key)

**Interfaces:**
- Produces:
  - `aws::Substrate { pub instance_profile_name: String, pub scheduler_role_arn: String, pub security_group_id: String, pub subnet_id: String }`
  - `AwsContext::ensure_substrate(&self, budget_alarm_usd: Option<u32>) -> Result<Substrate, Error>`
    — every sub-step is get-then-create under a deterministic name; running it on a fresh
    account and on the thousandth run is the same code path:
    1. Role `burst-actions-instance` (trust `ec2.amazonaws.com`, **no** policies — the
       near-empty profile of invariant 4) + instance profile `burst-actions-instance` +
       `AddRoleToInstanceProfile` (tolerate `LimitExceeded`/already-attached as
       already-done).
    2. Role `burst-actions-scheduler` (trust `scheduler.amazonaws.com`) with inline policy
       `burst-actions-terminate`: `ec2:TerminateInstances` on `instance/*` with
       `"Condition": {"StringEquals": {"aws:ResourceTag/burst-actions": "1"}}` — the role can
       kill only ours.
    3. Security group name `burst-actions` in the default VPC, created with
       `TagSpecifications` `burst-actions=1` (required by the policy's
       `CreateZeroInboundSecurityGroup` Sid), **zero ingress rules added ever** (a new SG is
       zero-inbound by default; we hold no rule-editing permission — the absence is
       IAM-enforced). Get-or-create by `DescribeSecurityGroups` filter `group-name` +
       `vpc-id`.
    4. If `budget_alarm_usd` is `Some(n)`: get-or-create budget `burst-actions-monthly`
       (monthly cost budget, limit `n` USD) via `DescribeBudget`/`CreateBudget` on the
       `budgets` client (global endpoint, us-east-1). Opt-in only — `None` makes zero
       Budgets calls.
  - Config: `BurstTable`/`Config` gain `budget_alarm_usd: Option<u32>` (absent = no alarm;
    docs comment names $15 as the suggested value per design §3 layer 5). Unknown-key and
    zero-value tests extended accordingly (`Some(0)` rejected).
- Consumes: policy Sids `SubstrateRoles`, `PassOurRolesToTheirServices`,
  `CreateZeroInboundSecurityGroup`, `CreateSecurityGroupInVpc`, `TagOnlyAtCreation`,
  `OptInBudgetAlarm` — nothing beyond them.

**Steps:**

- [ ] **Step 1:** Failing unit tests for the pure parts: the two trust-policy JSON documents
  and the scheduler inline policy rendered by
  `fn trust_policy(service: &str) -> String` / `fn scheduler_kill_policy() -> String` —
  assert exact service principals and the `aws:ResourceTag/burst-actions = "1"` condition
  (parse with `serde_json`, compare `Value`s, not strings).
- [ ] **Step 2:** Implement get-or-create: each `Get*` mapping `NoSuchEntity`/not-found to
  "create it", any other error to `Error::Aws { op, .. }` naming the failed call.
- [ ] **Step 3:** Config key + tests; all four checks; commit —
  `feat: ensure_substrate — instance-profile role, tag-fenced scheduler role, zero-inbound SG, opt-in budget alarm`.
- [ ] **Step 4 (lead, live):** run `ensure_substrate` twice against the real account (via the
  Task 8 `bake` path or a throwaway `#[ignore]` test run explicitly): first run creates, second
  is a no-op — verified by `aws iam get-role --role-name burst-actions-instance`,
  `get-role --role-name burst-actions-scheduler`, and
  `aws ec2 describe-security-groups --filters Name=group-name,Values=burst-actions` showing one
  SG, zero ingress rules. Report verified-vs-inferred accordingly.

---

### Task 5: AWS backend — `launch` / `terminate` / `list_tagged` + IAM-propagation retry

**Files:**
- Modify: `src/cloud/aws.rs`

**Interfaces:**
- Consumes (quoted from `src/cloud/mod.rs`, unchanged):

```rust
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub count: u32,
    pub image_id: String,
    pub instance_type: String,
    pub spot: bool,
    pub tags: TagSpec,
    pub user_data: String,
}
```

  and `TagSpec::to_tags(&self) -> [(String, String); 3]` from `src/schema.rs`.
- Produces: `aws::AwsCloud { ctx: AwsContext, substrate: Substrate }` implementing `Cloud`.
  - **`launch`**: one `RunInstances` with `min_count = max_count = spec.count`;
    `TagSpecifications` for **both** `ResourceType::Instance` and `ResourceType::Volume`, each
    carrying exactly `spec.tags.to_tags()` (atomic — no untagged window, and the policy's
    `LaunchTaggedOnly` Sid *denies* an untagged launch — the gate verifies that denial);
    `instance_initiated_shutdown_behavior(ShutdownBehavior::Terminate)`;
    `metadata_options(HttpTokens::Required)`; `security_group_ids([substrate.security_group_id])`;
    `subnet_id`; `iam_instance_profile(name = substrate.instance_profile_name)`;
    `user_data(base64(spec.user_data))`; `spot` → `InstanceMarketOptions` market type Spot,
    one-time, terminate on interruption. Wrapped in the IAM retry (below).
  - **`terminate`**: prove-ownership-then-destroy — `DescribeInstances` for the ids, filter
    `tag:burst-actions=1`; any requested id **not** in the verified set →
    `Error::Aws { op: "terminate", message }` naming the unverified id, terminating nothing;
    then one `TerminateInstances` with the verified ids.
  - **`list_tagged`**: `DescribeInstances` with filters
    `tag:burst-actions = 1` **and** `tag:burst-actions-repo = <repo>` **and**
    `instance-state-name in [pending, running, shutting-down, stopping, stopped]`
    (everything non-terminated), paginated to exhaustion. Semantics mirror the fake exactly —
    `FakeCloud::list_tagged` filters on both tag keys and its
    `list_tagged_excludes_instances_missing_burst_tag` test is the parity oracle: an instance
    carrying the repo tag but not `burst-actions=1` must not be listed by either backend.
    State mapping is an exhaustive match over the EC2 state name into `InstanceState`; an
    unrecognized state name is `Error::Aws`, never a silent skip.
  - **IAM eventual-consistency retry** (design §5), pure and unit-tested:

```rust
/// True for the transient invalid-instance-profile error RunInstances returns
/// in the seconds after ensure_substrate() creates the role on a fresh
/// account (AWS IAM is eventually consistent). Verified live at the phase-2
/// gate; the message fragment is AWS's, not ours.
pub(crate) fn is_iam_propagation_error(code: &str, message: &str) -> bool {
    code == "InvalidParameterValue" && message.contains("Invalid IAM Instance Profile")
}
```

    and `fn retry_delays() -> impl Iterator<Item = Duration>` — exactly
    `[2, 4, 8, 15, 15, 15]` seconds (bounded, ~60 s total). Only errors classified true are
    retried; exhaustion returns the underlying error wrapped with "IAM role propagation did
    not settle after 60s — retry `burst bake`".

**Steps:**

- [ ] **Step 1:** Failing unit tests: `is_iam_propagation_error` true for the pair above,
  false for (`InvalidParameterValue`, other message), (`UnauthorizedOperation`, anything);
  `retry_delays` sums to 59 s and has 6 entries; the EC2-state-name mapping covers all six
  documented names and errors on `"weird"`.
- [ ] **Step 2:** Implement the three methods + `Cloud` impl (`arm_kill`/`bake` return
  `Error::Aws { op, message: "not implemented until task 6/8" }` temporarily — replaced within
  this plan, never shipped past it).
- [ ] **Step 3:** All four checks; commit —
  `feat: AwsCloud launch/terminate/list_tagged — atomic tags, IMDSv2, ownership-verified terminate, bounded IAM retry`.

---

### Task 6: `arm_kill` — EventBridge Scheduler one-shot

**Files:**
- Modify: `src/cloud/aws.rs`

**Interfaces:**
- Consumes: trait method
  `fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error>`.
- Produces, pure and unit-tested:

```rust
/// Schedule name for an instance's one-shot kill: burst-actions-<instance-id>.
pub(crate) fn kill_schedule_name(instance_id: &str) -> String {
    format!("burst-actions-{instance_id}")
}

/// EventBridge Scheduler one-shot expression: at(yyyy-mm-ddThh:mm:ss), UTC, no zone suffix.
pub(crate) fn at_expression(at: DateTime<Utc>) -> String {
    format!("at({})", at.format("%Y-%m-%dT%H:%M:%S"))
}
```

  Implementation: `CreateSchedule` with `name = kill_schedule_name(id)`, group `default`
  (matching the policy ARN `schedule/default/burst-actions-*`),
  `schedule_expression = at_expression(at)`, `schedule_expression_timezone("UTC")`,
  `FlexibleTimeWindow { mode: Off }`, `ActionAfterCompletion::Delete` (fires once,
  self-deletes — nothing standing), target:
  `Arn = "arn:aws:scheduler:::aws-sdk:ec2:terminateInstances"`,
  `RoleArn = substrate.scheduler_role_arn`,
  `Input = {"InstanceIds": ["<id>"]}` (serialized via `serde_json`, not string
  concatenation). `ConflictException` (schedule exists) is success — arming is idempotent per
  instance. Also wrapped in a scheduler-flavoured propagation retry: `ValidationException`
  whose message contains `assume` on the role → same `retry_delays()` schedule (fresh-account
  case; message fragment verified at the gate).

**Steps:**

- [ ] **Step 1:** Failing unit tests: `kill_schedule_name("i-0abc") == "burst-actions-i-0abc"`;
  `at_expression` of `2026-08-08T18:00:00Z` == `"at(2026-08-08T18:00:00)"` (no `Z` — the API
  rejects zone suffixes); target input JSON round-trips to `{"InstanceIds":["i-0abc"]}`.
- [ ] **Step 2:** Implement; replace the task-5 stub.
- [ ] **Step 3:** All four checks; commit —
  `feat: arm_kill — burst-actions-<id> one-shot TerminateInstances schedule, self-deleting`.

---

### Task 7: VM payload — provisioning template, wrapper, systemd timers

**Files:**
- Create: `vm/provision.sh.tmpl`, `vm/burst-runner.sh`, `vm/units/burst-runner.service`,
  `vm/units/burst-bootstrap-deadline.timer`, `vm/units/burst-bootstrap-deadline.service`,
  `vm/units/burst-ttl.timer`, `vm/units/burst-ttl.service`; Create: `src/payload.rs`;
  Modify: `src/lib.rs` (`pub mod payload;`)

**Interfaces:**
- Consumes: `schema::ImageKeyInputs<'a> { pub provisioning_script: &'a [u8], pub base_image_id: &'a str, pub arch: Arch, pub runner_agent_version: &'a str }`
  and `schema::image_key(&ImageKeyInputs) -> String`.
- Produces (`src/payload.rs`):
  - `payload::render_provision(idle_timeout_min: u32, ttl_hours: u32, agent_version: &str) -> Result<String, Error>`
    — embeds `vm/provision.sh.tmpl` **and all unit/wrapper files** via `include_str!`,
    substitutes `__BURST_IDLE_TIMEOUT_MIN__`, `__BURST_TTL_HOURS__`,
    `__BURST_AGENT_VERSION__`; any placeholder left unsubstituted after rendering →
    `Error::Environment { reason }` naming it. **The rendered bytes are the
    `provisioning_script` image-key input** — so changing a timeout in `burst.toml` changes
    the key and forces a consistent rebake; on-VM timers can never drift from config
    silently.
  - `payload::fleet_user_data(jit_config: &str) -> String` — the per-VM launch user-data:
    writes `/etc/burst/jitconfig` (mode 0600) and `systemctl start burst-runner.service`.
    The timers are **enabled at bake and arm at every boot regardless of user-data** —
    broken/absent user-data still hits the bootstrap deadline (invariant 3 in miniature).
- Payload contents (all real files, key lines shown):
  - `provision.sh.tmpl` (`set -euo pipefail`): apt install of build essentials, `rustup`
    (for the `ubuntu` user), `chromium-browser firefox xvfb` + X libs, download
    `actions-runner-linux-x64-__BURST_AGENT_VERSION__.tar.gz` from
    `https://github.com/actions/runner/releases/download/v__BURST_AGENT_VERSION__/` into
    `/opt/actions-runner` owned by `ubuntu`, `./bin/installdependencies.sh`; install the
    unit files and `burst-runner.sh` (with both timeout placeholders substituted into the
    installed copies); `systemctl enable burst-bootstrap-deadline.timer burst-ttl.timer`;
    **not** `burst-runner.service` (started per-boot by user-data only). `--disableupdate`
    lives in the wrapper's run line.
  - `burst-runner.sh`: reads `/etc/burst/jitconfig`; runs
    `/opt/actions-runner/run.sh --jitconfig "$(cat /etc/burst/jitconfig)" --disableupdate`
    piping stdout through a monitor that touches `/run/burst/registered` on
    `"Listening for Jobs"` and `/run/burst/job-started` on `"Running job"`; a background
    watchdog sleeps `__BURST_IDLE_TIMEOUT_MIN__` minutes and, if `/run/burst/job-started`
    is absent (never-assigned), `systemctl poweroff`; when `run.sh` exits (one job done),
    `systemctl poweroff`.
  - `burst-bootstrap-deadline.timer`: `OnBootSec=10min`, `AccuracySec=30s`; its service:
    `[ -f /run/burst/registered ] || systemctl poweroff` — registered-or-die.
  - `burst-ttl.timer`: `OnBootSec=__BURST_TTL_HOURS__h`; service: unconditional
    `systemctl poweroff` — the hard cap.
  - Because the fleet launches with `instance-initiated-shutdown-behavior=terminate`, every
    `poweroff` above **is** termination: billing stops with zero IAM on the VM.

**Steps:**

- [ ] **Step 1:** Failing unit tests in `src/payload.rs`: rendered script contains no
  `__BURST_` substring; contains `--disableupdate`, `10` (via a render with 10/6), and the
  agent version; rendering with a template hand-corrupted in-memory (helper taking the
  template string) with a bogus `__BURST_TYPO__` errors naming it;
  `fleet_user_data("blob")` contains `blob`, `chmod 600`-equivalent mode line, and
  `systemctl start burst-runner.service`; two renders with different `ttl_hours` produce
  different `image_key` outputs (composing with `schema::image_key` — the config-change ⇒
  rebake property pinned as a test).
- [ ] **Step 2:** Author the `vm/` files and `payload.rs`. `shellcheck vm/*.sh*` clean if
  shellcheck is installed (record whether it ran).
- [ ] **Step 3:** All four checks; commit —
  `feat: vm payload — provisioning template, one-job wrapper, bootstrap/idle/TTL timers; timeouts enter the image key`.

---

### Task 8: `burst bake` — end-to-end with one-generation cache

**Files:**
- Create: `src/commands/mod.rs`, `src/commands/bake.rs`; Modify: `src/main.rs`,
  `src/lib.rs` (`pub mod commands;`), `src/schema.rs`, `src/cloud/aws.rs`

**Interfaces:**
- Consumes: `config::load(dir: &Path, repo_flag: Option<&str>) -> Result<Config, Error>` and
  `Config { pub repo, pub instance_type, pub region, pub arch, pub base_ami: Option<String>, pub idle_timeout_min, pub ttl_hours, .. }`;
  `github::Client`; `payload::render_provision`; `AwsContext`; trait method
  `fn bake(&mut self, key: &str) -> Result<String, Error>`.
- Produces:
  - `src/schema.rs`: `pub const TAG_IMAGE_KEY: &str = "burst-actions-image-key";`
  - `commands::bake::run(config: &Config) -> Result<(), Error>` orchestrating:
    1. `github::token_from_env()` + `runner_agent_version()` (fail before any AWS resource
       exists if the PAT is bad — GitHub-down-at-launch aborts clean).
    2. `AwsContext::connect(config.region)`; `ensure_substrate(config.budget_alarm_usd)`.
    3. Base AMI: `config.base_ami` **is the pin** (§8.6). If absent: resolve the current
       Ubuntu 24.04 LTS AMI for `config.arch` via `DescribeImages`
       (owner `099720109477`, name
       `ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*`, newest by creation
       date — read-only, in policy) and **fail loud**:
       `no base_ami pinned: set base_ami = "<resolved-id>" in burst.toml (current Ubuntu 24.04 LTS <arch> in <region>)`
       — the remedy is copy-pasteable and the pin stays a deliberate act.
    4. `render_provision(...)` → `image_key(&ImageKeyInputs { provisioning_script: rendered.as_bytes(), base_image_id, arch, runner_agent_version })`.
    5. `AwsCloud::bake(&key)`.
  - `AwsCloud::bake(&mut self, key: &str) -> Result<String, Error>`:
    1. **Cache check**: `DescribeImages` owner `self`, filters `tag:burst-actions=1`,
       `tag:burst-actions-image-key=<key>`, `state=available` → hit returns the image id
       (prints `image cache hit: <ami> (<key>)`), no builder launched. Same
       get-or-create-per-key semantics the fake's `bake_is_get_or_create_per_key` test
       pins.
    2. Miss: `RunInstances` builder — tag triple via `TagSpec { repo, expires: now + 1h }`
       on instance+volume, `shutdown_behavior = Stop` (the one deliberate exception to
       `terminate`, so `CreateImage` has a stopped instance; the kill schedule still
       terminates a stopped instance), IMDSv2, our SG/subnet/profile; user-data = the
       rendered provisioning script wrapped so that success ends in
       `touch /var/lib/burst/provisioned && poweroff` and failure leaves the instance
       running (no marker, no poweroff — the CLI timeout catches it; no SSM permission
       exists to ask). **Immediately** `arm_kill(builder_id, now + 1h)` — the builder is
       kill-armed before any waiting begins, so a SIGKILLed CLI leaks only a
       schedule-reaped instance.
    3. Poll `DescribeInstances` every 15 s until state `Stopped`; > 25 min →
       `Error::BakeTimeout` (terminate builder tag-verified + `DeleteSchedule` first, as
       the message promises).
    4. `CreateImage` name `burst-actions-<key>` with `TagSpecifications` for
       `ResourceType::Image` **and** `ResourceType::Snapshot`: `burst-actions=1`,
       `burst-actions-repo=<repo>`, `burst-actions-image-key=<key>`.
    5. Poll `DescribeImages` until `available` (30 s interval, 20 min cap).
    6. Terminate the builder (ownership re-verified) — its schedule self-deletes or is
       deleted explicitly.
    7. **One generation**: `DescribeImages` owner `self`, `tag:burst-actions=1`,
       `tag:burst-actions-repo=<repo>`, image-key **≠** new key → for each, re-verify
       `burst-actions=1` in the returned tags before acting, then `DeregisterImage` and
       `DeleteSnapshot` for each block-device snapshot id.
  - `src/main.rs`: `Cmd::Bake` arm calls `commands::bake::run(&config)`; error → `error: {e}`
    to stderr, exit 1. All other arms stay `not_implemented`.

**Steps:**

- [ ] **Step 1:** Failing unit tests: superseded-image selection as a pure fn
  `fn superseded<'a>(images: &'a [(String /*id*/, Option<String> /*key tag*/)], keep_key: &str) -> Vec<&'a String>`
  — keeps the matching key, selects others **including key-tag-missing images** (tag-verified
  burst images without a readable key are stale by definition); builder user-data wrapper
  contains `provisioned && poweroff` and no bare `poweroff` on the failure path; existing CLI
  test `subcommands_fail_loud_not_silent` updated to drop `"bake"` from the
  not-implemented list (it now fails differently offline — with a GitHub-token or AWS error,
  still exit 1, still loud; assert that).
- [ ] **Step 2:** Implement `commands::bake` + `AwsCloud::bake`.
- [ ] **Step 3:** All four checks (offline: everything green with no credentials); commit —
  `feat: burst bake — cache-keyed builder bake, kill-armed from launch, one-generation GC`.

---

### Task 9: Phase gate — live verification (lead-run, watched, not `cargo test`)

**Files:** none created; findings recorded in the session report and, where wording changes,
`docs/permissions.md` (via James/lead review, not silently).

Everything below is executed manually by the lead on the real account. **Every instance uses
`instance_type = "t3.micro"` in the gate `burst.toml`; every launch path already applies the
tag triple + armed kill schedule.** Distinguish "I ran it and saw X" from inference in the
report for each line.

The sweep-equivalent listing that ends **every** session:

```
aws ec2 describe-instances \
  --filters Name=tag:burst-actions,Values=1 \
            Name=instance-state-name,Values=pending,running,shutting-down,stopping,stopped \
  --query 'Reservations[].Instances[].[InstanceId,State.Name,Tags]' --output table
aws scheduler list-schedules --name-prefix burst-actions- --query 'Schedules[].Name'
```

Expected at session end: empty table, empty schedule list — stated in the report.

- [ ] **Gate 1 — fresh-account bake, idempotence, rebake**: `burst bake` on the empty account
  creates roles/SG/AMI; verify by AWS listing
  (`describe-images --owners self --filters Name=tag:burst-actions,Values=1` shows one AMI
  with the key tag; IAM/SG listings from Task 4 Step 4), **not** by exit code. Re-run →
  `image cache hit`, zero new resources (listing unchanged). Edit `ttl_hours` in
  `burst.toml` → rebake fires (key changed — the Task 7 test predicted this; now watch it),
  new AMI appears, superseded AMI **and its snapshot** are gone from the listing.
- [ ] **Gate 2 — boot-to-registered < 2 min, measured**: mint via
  `cargo run --example mint_jit -- <repo> gate-vm-1`; launch one `t3.micro` from the baked
  AMI through the tool's launch path (throwaway `#[ignore]` test or small example calling
  `AwsCloud::launch` + `arm_kill` with `payload::fleet_user_data(<blob>)` — never a raw
  untagged `aws ec2 run-instances`); record `RunInstances` timestamp and the
  Settings→Runners "Idle" timestamp; compute and report the delta. Job-drain not required
  this phase; the VM self-terminates via idle timeout (watch that too — it's layer 3's
  never-assigned case firing for free).
- [ ] **Gate 3 — bootstrap deadline watched firing**: same launch with
  `fleet_user_data("garbage-not-a-jitconfig")`; observe poweroff-as-termination at ~10 min
  (instance state → `terminated` without any kill from us). Record actual elapsed time.
- [ ] **Gate 4 — interrupted bake reaped by the schedule**: start `burst bake` after
  touching the provision template (cache miss), `kill -9` the CLI once the builder is
  running; verify the only residue is one tagged instance + one `burst-actions-i-*`
  schedule; **wait and watch** the schedule terminate it at the 1 h mark and self-delete
  (`ActionAfterCompletion=DELETE` verified by the schedule disappearing from
  `list-schedules`). Then delete the orphaned half-baked state by re-running `burst bake`.
- [ ] **Gate 5 — IAM policy vs `docs/permissions.md`**: the credentials in use are exactly the
  documented policy — Gates 1–2 running under them is the positive verification. Additionally
  verify the deny-side: one deliberate untagged `RunInstances --dry-run` via the aws CLI is
  **denied** — the `LaunchTaggedOnly` fence is real. Record every call that failed or needed
  different wording (resource-level quirks are expected per that doc's preamble); wording
  fixes go to `docs/permissions.md` as a reviewed commit —
  `docs/permissions: wording fixes from live phase-2 verification`. Scope growth is a
  finding for James, never a silent edit.
- [ ] **Gate 6 — IAM propagation retry observed (fresh-account only)**: on the first-ever
  bake, note whether the retry classifier fired (log line per retry attempt names the
  matched error); if the live error text differs from
  `"Invalid IAM Instance Profile"`, fix the classifier + its unit test and record the real
  text as the cached external fact.
- [ ] **Gate 7 — zero-residue close**: run the sweep-equivalent listing above; empty; state
  it in the report. Full offline suite + all four checks green on the final tree.

---

## Phase gate checklist (from implementation-phases.md, watched)

- [ ] `burst bake` on an empty account creates everything and produces a tagged AMI; re-run
  is a cache-hit no-op; config edit → rebake + superseded-image deletion — verified by AWS
  listing, not exit code (Gate 1).
- [ ] Manually launched VM from the AMI with a minted JIT config reaches "registered" on
  GitHub in < 2 min from `RunInstances` — measured, not asserted (Gate 2).
- [ ] Bootstrap deadline watched firing once on a garbage JIT config (Gate 3).
- [ ] Builder interrupted mid-bake leaves only a tagged, kill-scheduled instance that the
  schedule then reaps — watched (Gate 4).
- [ ] Every instance this phase created carried the tag triple + armed schedule from launch;
  session ends with a sweep-equivalent listing showing zero live instances (Gates 2–4, 7).
- [ ] `docs/permissions.md` verified against live AWS behavior; wording fixes recorded
  (Gate 5).
