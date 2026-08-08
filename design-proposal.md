# Burst CI runners — design proposal

**Status: PROPOSAL.** Major axes decided (§6); nothing built. Open questions in §8.

## 1. Requirements

James runs one fast persistent self-hosted GitHub Actions runner at home. Everyday CI fits it;
heavy-but-parallelizable work (browser benchmarks, xvfb-static builds) is forced serial by it.
Wanted: **one command** that launches cloud VMs as extra runners, lets them drain the job queue,
then terminates and cleans up everything.

Hard requirements:
- **Scale to zero** — nothing runs and nothing bills (beyond pennies of storage) at idle.
- **Cleanup guaranteed, not best-effort** — no leaked VMs, regardless of what fails mid-run.
- **Operationally simple and durable** — solo maintainer, understandable in one sitting, must
  still work in two years untouched.
- **Zero AWS account pre-setup** — works given only AWS + GitHub credentials; everything else the
  tool creates itself. Configuration beyond credentials is optional.
- **No Terraform/Ansible dependency** (available in the homelab, disfavored).
- AWS first; cross-cloud a bonus. Prefer a mature existing tool over invention — but a small owned
  tool beats an enterprise system.

This doc synthesizes three commissioned takes (see `research/`): a homelab-ecosystem survey, a
GitHub-Actions-ecosystem survey, and a first-principles design.

## 2. Landscape verdict: build; RunsOn is the buy-fallback

All three takes converged on the same core mechanism (ephemeral runners + prebaked image +
instance self-termination + independent kill switch). Every existing *package* of it fails a hard
requirement:

| Option | What it is | Why not |
|---|---|---|
| **actions-runner-controller** (GitHub-official) | k8s controller, runner pods | Needs a k8s cluster running 24/7. |
| **terraform-aws-github-runner** (ex-philips-labs) | Lambda+SQS webhook autoscaler | Standing multi-Lambda control plane, Terraform-based, built for sustained team volume; repo archived/migrated orgs in 2025. |
| **GARM** (cloudbase) | Multi-cloud pool daemon | An always-on service to babysit, automating a demand signal the human already has. |
| **machulav/ec2-github-runner**, **ubergeek77/aws-ec2-spot-runner** | Workflow steps that start/stop EC2 | Zero standing infra (right spirit), but runner lifetime is tied to one workflow run and cleanup rides on a `stop` job a cancelled run can skip. Kept as reference implementations. |
| **RunsOn** (runs-on.com) | CloudFormation stack in your own account; per-job ephemeral VMs; free for non-commercial | The credible buy option: no k8s, scale-to-zero, actively maintained, no vendor-hosted control plane. Rejected only because it's third-party code holding EC2 rights in our account and the small owned alternative exists. **Fallback: if the DIY tool palls, adopt RunsOn rather than growing it.** |
| Hosted vendors (WarpBuild, Namespace, …) | Rent their fleet | Vendor-existential risk (BuildJet and Cirrus CI both died H1 2026); spot EC2 is ~2.7× cheaper than GitHub's hosted large runners. |

The pattern is the same one every serious system above uses internally — which is what makes a
small owned implementation safe to own.

## 3. Design

### Shape

One standalone Rust binary, `burst` (on `aws-sdk-rust`), plus ~120 lines of bash + three systemd
units baked into an AMI. Subcommands: `up [N|--auto] [--spot]`, `bake`, `status`, `down`, `sweep`.

**At idle there exists:** one AMI + its EBS snapshot, two IAM roles (instance profile; EventBridge
Scheduler execution role), one security group, an opt-in budget alarm, and — phase 2 — one S3
cache bucket. Nothing executes. Cost ≈ the snapshot: ~$1.25–2/month (§ image).

**Zero pre-setup:** every standing resource is get-or-create under deterministic `beep-burst-*`
names — `ensure_substrate()` runs at the start of every invocation, so a fresh AWS account and the
thousandth run are the same code path. A fresh account simply has no AMI, which is an image-cache
miss: the first `burst up` bakes, then launches. Required inputs: AWS credentials (env/profile)
and a GitHub fine-grained PAT (self-hosted-runners:write on the target repo). Optional `[burst]`
TOML block for instance type, timeouts, max fleet, region — all with working defaults.

### Invariants

1. **Scale to zero.** No daemon, webhook receiver, Lambda, or warm pool. Ever.
2. **Tag or it doesn't exist.** Every instance carries `beep-burst=1` and
   `beep-burst-expires=<ISO8601>`, applied atomically inside `RunInstances`. Anything tagged and
   past expiry is terminable by anyone without inspection. The cloud is the state; tags are the
   schema; no state file exists to disagree with reality (also why no Terraform).
3. **Termination needs no cooperation.** No cleanup path requires the VM's OS, the GitHub API, or
   the launching CLI to be alive.
4. **A VM never holds a credential that outlives it or exceeds it.** Its only GitHub material is
   a single-use JIT config; its instance profile is scoped to (eventually) one S3 prefix.
5. **Untrusted code never reaches a runner.** Labels route jobs but are not a trust boundary — a
   fork can edit `runs-on:`. The repo's Actions setting must require approval for outside
   collaborators' workflows; `burst up` hard-errors if it's weaker.

### Registration: JIT + ephemeral, one VM per job

Each runner is registered via GitHub's `generate-jitconfig` API: the CLI (holding the PAT, which
never leaves the dev machine) mints one single-use blob per VM; the runner starts as
`run.sh --jitconfig <blob>`, makes zero registration API calls, executes **exactly one job**, and
GitHub deregisters it automatically. One VM = one runner = one job = one boot (~60–90s from a
prebaked AMI — the cost that makes this trade acceptable).

Rejected alternatives: a classic registration token with multi-job runners (carries a ≤1h token
on the VM that could register rogue runners against the repo); JIT with on-VM re-minting (puts a
minting credential on a machine that executes CI code — worse than what JIT avoids).

What this buys beyond the credential story: GitHub-side registration GC is automatic (the sweep
only covers runners that never connected), and every job gets a fresh filesystem.

### Lifecycle

```
burst up N:
  ensure_substrate()              # get-or-create roles / SG / alarm (idempotent)
  sweep()                         # reap anything expired — rent paid on entry
  check fork-approval setting     # invariant 5
  ensure AMI (image-cache key)    # hit → proceed; miss → bake inline
  mint N JIT configs
  RunInstances × N                # tags in TagSpecifications (no untagged window),
                                  #   instance-initiated-shutdown-behavior=terminate,
                                  #   JIT config in user-data, IMDSv2 required
  arm N EventBridge Scheduler one-shots: TerminateInstances(id) at launch+TTL,
                                  #   ActionAfterCompletion=DELETE
  exit                            # the CLI is not a supervisor; killing it leaks nothing
```

On the VM, cloud-init arms watchdogs BEFORE starting the runner: a TTL-hour absolute-lifetime
poweroff and a 10-minute registered-or-poweroff bootstrap deadline. Then the runner runs its one
job and exits → poweroff. A VM that boots into an already-drained queue (or whose labels match
nothing) is caught by the never-assigned idle timeout → poweroff. Because of
shutdown-behavior=terminate, **poweroff IS termination**: billing stops, and self-cleanup needs
zero IAM permissions on the VM.

The queue is never polled to decide shutdown: the fleet is sized to the queue at launch, each VM
dies with its job, and a GitHub-unreachable partition is indistinguishable from an empty queue —
same correct outcome, the VM dies. (Why self-managing: an external decider must stay alive, reach
both APIs, and infer remote state — three failure surfaces, one violating scale-to-zero. The VM
needs local facts and a syscall.)

### Cleanup: defense in depth, ranked by reliability

1. **EventBridge Scheduler one-shot kill at launch+TTL** — pure AWS control plane; survives
   kernel hang, GitHub outage, CLI death, vacation; self-deletes after firing. (The research
   reports prescribe a standing scheduled reaper here; the per-instance one-shot gives the same
   guarantee with nothing standing.)
2. **`instance-initiated-shutdown-behavior=terminate`** — set before boot; the instance profile
   can't call ModifyInstanceAttribute, so nothing on the VM can undo it.
3. **On-VM systemd timers** — bootstrap deadline, never-assigned idle timeout, TTL cap.
4. **Sweep on every invocation** — terminate tagged-and-expired instances, delete orphan
   schedules, delete never-connected runner registrations. Idempotent; driven entirely by tags.
5. **Budget alarm (opt-in, ~$15/mo threshold)** — not a cleanup layer; the tripwire ensuring a
   bug in all four costs days of pennies, not a month of silence.

Failure modes and which layer catches them: CLI killed mid-launch → 2/3/4 (and tagging is atomic
with launch, so no untagged window); cloud-init or registration failure → bootstrap deadline;
kernel wedge → 1; hung job → TTL cap, then 1 (GitHub marks the job failed when the runner
vanishes — correct for a hung job); cancelled workflow → runner exits jobless → idle timeout;
GitHub API down at launch → JIT minting fails before any instance exists, clean abort; spot
reclaim → job fails visibly, instance terminates natively; sweep API errors → retried next
invocation, layer 1 is per-instance and independent; user never runs burst again → 1–3 are all
user-independent. Orphaned runner *registrations* are cosmetic, never billing; the sweep deletes
them and GitHub ages them out anyway.

### The image: a content-addressed cache, one generation

Boot-time dependency install would add 5–10 min to every VM — exactly the latency this tool
exists to remove — so CI dependencies are baked into an AMI, built by the tool itself (no
Packer/Ansible): launch a builder from stock Ubuntu/Debian, run the version-controlled
provisioning script (toolchain, runner agent, browsers, X stack, VM agent + units), optionally
warm `target/` via `cargo build` against main, `CreateImage`, terminate the builder. The builder
carries the same tags and kill-schedule as any fleet VM.

**The AMI persists between runs and rebakes only when its inputs change:**

- **Key** = hash(provisioning script ∥ base image ID ∥ arch ∥ runner agent version), stamped as a
  tag on the AMI.
- **`burst up`**: key hit → launch (~90s to registered workers, the common case); miss → bake
  inline (10–20 min), tag, launch, delete the superseded AMI + snapshot. `burst bake` forces it.
- **One generation kept** (config is versioned, so the image is reproducible; a rollback copy
  would insure only against extremes like an apt package vanishing upstream — don't pay until it
  happens). Idle cost: $0.05/GB-month × ~25–40 GB written blocks ≈ **$1.25–2/month**.
  Delete-after-run was rejected: it saves that ~$2/month by putting a serial bake in front of
  every burst.
- **The agent-version key term doubles as the staleness guard**: GitHub stops dispatching to
  runner agents ≳30 days old, and the image bakes `--disableupdate`. The CLI resolves the current
  agent release at `up` time, so each release (~monthly) is a cache miss and the fleet self-heals
  with a rebake — staleness never needs a warning.
- **Known decay the key won't notice**: the baked `target/` drifts stale against main without
  changing the key; builds slow gradually until the next natural rebake. Properly fixed by the
  phase-2 sccache tier; `--rebake` is the interim lever.

**Phase 2 — shared sccache (S3 backend, used by home runner and fleet):** keeps compile caches
warm between bakes and across the fleet. Deferred until fleet build times are measured, but the
one-job-per-VM model raises its expected value: there is no job-to-job cache reuse on a VM, so
every job starts from the baked `target/` alone.

### Fleet sizing and economics

**N independent VMs, one runner each — never one big box:** GitHub parallelism is per-runner
anyway; browser benchmarks fight over displays, ports, and CPU when co-tenanted; N common-size
instances launch more reliably than one rare 64-vCPU one; one failure costs 1/N.

With one job per VM, N is the number of queued jobs: `burst up --auto` counts queued
burst-labeled jobs (capped at `max_fleet`, default 12) — the primary UX; `burst up N` covers
launching ahead of a push. No mid-flight rescaling: grow by running `up` again; shrink is
automatic. Under-provisioning is benign — leftover jobs fall to the home runner or a second `up`.

**On-demand by default, `--spot` opt-in:** loop latency is the stated priority, and a spot
reclaim mid-benchmark taxes the loop to save single-digit dollars a month. `--spot` fits long
parametric sweeps where a lost shard is cheap to re-run.

### Routing and coexistence with the home runner

Burst-eligible jobs declare `runs-on: [self-hosted, burst]`. The home runner carries
`[self-hosted, linux, x64, home, burst]` — it also accepts burst work, so a stray benchmark job
never strands when no fleet is up (and because a registered runner with the `burst` label always
exists, GitHub queues such jobs rather than failing them for want of a matching runner). Everyday
CI targets `home`, which fleet VMs don't carry, so the fleet never steals the fast path. Labels
only; runner groups are org-scoped and add nothing here.

### Security

- **PAT** on the dev machine only, never on a VM. On a VM: one single-use JIT config (in
  user-data; IMDSv2 enforced) and the near-empty instance profile.
- **Network:** zero-inbound security group (the runner long-polls outbound); no SSH keys by
  default (`--ssh-key` for debug sessions). Egress open: allow-listing was rejected — GitHub's
  CIDRs churn, benchmarks need the real web, and the fork-approval gate (invariant 5) carries the
  trust load. If the beep egress-firewall design (D24/S6) later lands a posture, the fleet should
  inherit it — flagged, not decided here.
- Perspective: the *home* runner — persistent, on the home LAN, accumulating state across jobs —
  is the scarier machine. Fleet VMs are fresh-imaged, isolated, and dead within hours.

## 4. What we are explicitly NOT building

No webhook autoscaler; no always-on anything; no k8s; no Terraform/Ansible/Packer; no separate
bootstrap step (the tool is its own installer); no mid-flight rescaling; no per-job microVMs; no
dashboard (`burst status` prints text). Multi-cloud stays a seam, not an implementation: a
five-method `Cloud` trait (`launch / terminate / list_tagged / arm_kill / bake`) keeps the door
open — GCP's native `max-run-duration` would even collapse cleanup layer 1 into the launch call —
but a second backend gets written when wanted, not before. The test for every exclusion: does its
failure mode cost more maintainer attention than its absence costs money or risk?

## 5. Implementation notes

Rust, standalone crate — **never a beep workspace member or xtask** — and keep a prebuilt binary
at hand: "CI is broken" must never block launching more CI runners. (These two rules preserve
what the first-principles doc's pro-Python argument was actually protecting; as a standalone
static binary, Rust is the more durable choice — no interpreter/venv drift, and the
failure-ordering logic is where the type system pays rent.) `aws-sdk-rust` covers everything used:
EC2, EventBridge Scheduler, IAM, STS, Budgets.

## 6. Decision log

All 2026-08-08, James:
1. **Build** the owned tool; RunsOn is the named fallback.
2. **JIT + ephemeral, one VM per job** (boot cost acceptable given a prebaked AMI).
3. **AMI kept between runs**, content-addressed by config hash; **one generation**.
4. **Rust** (`aws-sdk-rust`), standalone crate.
5. Zero AWS pre-setup; no Terraform/Ansible (litmus tests from the original request).

## 7. Rollout

1. **`burst bake` v1** — exercises `ensure_substrate()` on the fresh account; verify
   boot-to-registered < 2 min.
2. **`burst up/down/status/sweep` v1** — launch against a synthetic queued matrix, then
   deliberately kill-test every cleanup layer (kill the CLI mid-launch, boot a broken AMI, hang a
   job, cancel a run mid-flight) and watch each layer catch its case. The layers are only real
   once each has been observed firing.
3. **First real burst** — a browser-benchmark matrix on `runs-on: [self-hosted, burst]`, measured
   against the serial baseline.

## 8. Open questions

Defaults awaiting a call (all cheap to change after first contact):
1. **Timeouts** — never-assigned idle timeout 10 min; hard TTL 6 h.
2. **Instance type** — lean c7i.2xlarge-class; benchmarks may want bare-metal-ish consistency —
   measure first. Related: root gp3 volume size/IOPS for browser workloads.
3. **sccache tier** — now or after measuring (lean: after, but §3 raises its value).

Latent — likely to surface at execution time, no design change expected but each needs an answer:
4. **Multi-repo scope.** The repos live under a personal account, and org-level runners don't
   exist for user accounts — so registration is per-repo. If burst serves both the main repo and
   the benchmarks repo, the config needs a repo list, `--auto` must count queues across them, and
   each VM's JIT config pins it to one repo. Fleet-splitting heuristic when both have queued work?
5. **Arch.** x86_64 assumed (parity with the home runner matters for benchmark comparability);
   Graviton is cheaper if a workload is ever arch-agnostic. The image key already includes arch.
6. **EC2 vCPU quota.** A fresh account's on-demand vCPU quota can cap the fleet below
   `max_fleet`. `burst up` should detect the quota and say so rather than half-launching silently.
7. **Concurrent invocations.** Two `burst up`s racing is mostly benign (sweep is idempotent;
   over-provisioned VMs die on the idle timeout) but a simultaneous image-cache miss double-bakes
   — wasteful, not harmful. Accept or add a cheap bake lock (e.g. conditional tag write)?
8. **PAT lifetime.** Fine-grained PATs expire (max 1 year); the tool should fail with a clear
   "token expired, rotate it" rather than an opaque 401. A GitHub App would auto-rotate but adds
   setup — against the zero-pre-setup litmus. Accept annual manual rotation?
9. **Region.** Default from the AWS profile; the AMI is region-bound, so a region change is a
   full cache miss (fine, just worth knowing). Spot capacity varies by region/AZ if `--spot` is
   used.
10. **Provisioning config location.** The tool is project-agnostic; each project supplies its
    provisioning script + `[burst]` block. Where does discovery happen — a path flag, a
    well-known file in the invoking repo, or both?
