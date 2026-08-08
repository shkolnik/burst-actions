# Burst CI runners — design proposal

**Status: PROPOSAL, awaiting James's read.** Nothing here is built.

The need (James, 2026-08-08): the single persistent self-hosted runner is right for everyday CI, but
heavy-parallelizable work (browser benchmarks, xvfb-static) is forced serial by it. Wanted: **one
command** that launches cloud VMs from a predefined CI image, registers them as GitHub Actions
runners, lets them drain the queue, then unregisters, terminates, and cleans up everything —
**scale-to-zero** when idle, cleanup **guaranteed** not best-effort, simple and durable for years.
AWS preferred; cross-cloud a bonus. Mature existing tool preferred over invention — but a small
owned script beats an enterprise system. Additional constraints (James, same day): Terraform/Tofu
and Ansible exist in the homelab but are disfavored — don't depend on them; and (litmus test)
**minimal-to-no pre-setup of the AWS account** — the tool should work given only AWS + GitHub
credentials with the necessary permissions, with configuration beyond that optional, not required.

This doc synthesizes three independent takes commissioned for it: a homelab-ecosystem survey, a
GitHub-Actions-ecosystem survey, and a from-scratch first-principles design.

---

## 1. Verdict on the existing-tool landscape: nothing mature fits; the *pattern* is mature

All three takes, arriving independently, converged on the same core mechanism (§3). What differs is
packaging — and every existing package fails at least one hard requirement:

| Option | What it is | Why not |
|---|---|---|
| **actions-runner-controller** (GitHub-official) | k8s controller, ephemeral runner pods | Needs a k8s cluster running 24/7. A full-time platform for a part-time problem. |
| **philips-labs / github-aws-runners terraform-aws-github-runner** | Lambda+SQS webhook autoscaler, Terraform | Multi-Lambda standing control plane, Terraform-dependent, built for sustained team volume; original repo archived Jan 2025 and migrated orgs mid-flight — churn we don't need. |
| **GARM** (cloudbase) | Single Go daemon, multi-cloud pools | A daemon to run and babysit forever. (Interesting twist: it could live on the existing always-on runner box. Still rejected — it's a standing service whose failure modes accrue while nobody watches, to automate a demand signal the human already has.) |
| **machulav/ec2-github-runner**, **ubergeek77/aws-ec2-spot-runner** | GitHub Actions steps that start/stop EC2 from *within* a workflow | Right spirit (zero standing infra), wrong lifecycle: runner lifetime is tied to one workflow run, and cleanup rides on a `stop` job that a cancelled run can skip. Our need is "drain a queue across runs," not "bootstrap one run's runner." Kept as reference implementations. |
| **RunsOn** (runs-on.com) | CloudFormation stack in your own AWS account; per-job ephemeral VMs; free for non-commercial/OSS | The credible **buy** option: no k8s, scale-to-zero, actively developed through 2026, no vendor-hosted control plane (BuildJet and Cirrus CI both died in H1 2026 — self-hosting in our own account is the only vendor-risk-free shape). Rejected as first choice only because it's a third party's control-plane code running with EC2 rights in our account, per-job VM granularity is heavier than our multi-job-drain need, and the ~200-line owned alternative exists. **Named fallback:** if the DIY tool palls, adopt RunsOn rather than growing the script. |
| Hosted runner vendors (WarpBuild, Namespace, …) | Rent their fleet | Vendor-existential risk (see BuildJet/Cirrus), and spot EC2 is ~2.7× cheaper than even GitHub's post-2026-price-cut hosted large runners, with the gap widening at larger sizes. |

So: **build the small owned tool.** The pattern it implements is the same one every serious system
above uses internally, which is exactly what makes a ~800-line version safe to own.

## 2. The recommended design: `burst`

A single Python file (`ci/burst.py`, boto3, no Terraform/Ansible/Packer anywhere), plus ~120 lines
of bash + three systemd units baked into an AMI. **At idle there exists: one AMI + snapshot, one
IAM role, one security group, (later, optional) one S3 cache bucket, and a $-budget alarm. Nothing
executes; cost is pennies of EBS snapshot storage.**

**Zero pre-setup:** every one of those standing resources is created by the tool itself,
idempotently, under deterministic names (`beep-burst-*`). Each `burst` invocation begins with
`ensure_substrate()` — get-or-create the role, instance profile, security group, and (opt-in)
budget alarm; `burst up` on an AMI-less account tells you to run `burst bake` first, and that is
the entire onboarding. This falls straight out of invariant 2: names/tags are the schema and the
cloud is queried for what exists, so "first run on a fresh account" and "thousandth run" are the
same code path — get-or-create, not a bootstrap script and not IaC state. Configuration is a small
optional `[burst]` TOML block (instance type, idle timeout, max fleet, region) with working
defaults; the required inputs are exactly two credentials: AWS (env/profile) and the GitHub PAT.

The five invariants (from the first-principles design, adopted verbatim in spirit):

1. **Scale to zero** — no daemon, no webhook receiver, no Lambda, no warm pool. Ever.
2. **Tag or it doesn't exist** — every instance carries `beep-burst=1` and
   `beep-burst-expires=<ISO8601>`, applied atomically in `RunInstances`. Anything so tagged past
   expiry is terminable by anyone without inspection. **The cloud is the state; tags are the
   schema; there is no state file to disagree with reality** (this is also why no Terraform).
3. **Termination needs no cooperation** — the deepest cleanup layer is pure AWS control plane;
   no cleanup path requires the VM's OS, the GitHub API, or the launching script to be alive.
4. **The VM never holds a credential that outlives it or exceeds it.**
5. **Untrusted code never reaches a runner** — fork-PR workflows require explicit approval
   before any job runs; `burst up` hard-errors if the repo's Actions approval setting is weaker
   (labels route jobs, they are not a trust boundary — a fork can edit `runs-on:`).

### Lifecycle — the VM is self-managing

```
burst up N:
  sweep()                         # cleanup is rent paid on entry, every invocation
  check fork-approval setting     # invariant 5
  mint runner registration        # GitHub credential never leaves the dev machine
  RunInstances × N                #   prebaked AMI, tags in TagSpecifications,
                                  #   instance-initiated-shutdown-behavior=terminate
  arm per-instance EventBridge Scheduler one-shot: TerminateInstances at launch+6h,
                                  #   ActionAfterCompletion=DELETE (self-deleting)
  exit                            # the CLI is NOT a supervisor; killing it leaks nothing
```

On the VM (cloud-init, all armed BEFORE the runner starts): a 6-hour absolute-lifetime
`systemd-run` poweroff; a 10-minute registered-or-poweroff bootstrap deadline; then
`config.sh --unattended --disableupdate --labels burst` and the runner service. Runner job hooks
touch `/run/last-activity`; a 1-minute timer powers off when
`!busy && now − last_activity > IDLE_TIMEOUT`. Because shutdown-behavior=terminate, **`poweroff`
IS termination and billing stops — self-cleanup requires zero IAM permissions on the VM.**

"Queue is empty" is never queried: an idle runner past its timeout *is* the queue-empty signal —
GitHub dispatches queued matching jobs to idle runners, so idle+queued-work cannot coexist. A
GitHub-unreachable partition is indistinguishable from an empty queue and produces the same,
correct outcome: the VM dies.

Why self-managing rather than an external manager: an external decider must stay alive, reach both
APIs, and correctly infer remote state — three failure surfaces, one of which violates
scale-to-zero. The VM deciding its own death needs local facts and a syscall.

### Cleanup: five independent layers, ranked by reliability

1. **EventBridge Scheduler one-shot kill at launch+6h** — pure control plane; survives kernel
   hang, GitHub outage, CLI death, vacation. Self-deletes after firing. (Adjudication note: both
   research reports prescribe a *standing* scheduled reaper Lambda here; the per-instance
   self-deleting one-shot achieves the same guarantee with nothing standing — adopted instead.)
2. **`instance-initiated-shutdown-behavior=terminate`** — set before boot; unremovable from
   inside (the instance role can't call ModifyInstanceAttribute); turns every OS poweroff into
   termination.
3. **On-VM systemd timers** — 10-min never-registered deadline, 6h cap, 1-min idle check.
4. **Sweep on every `burst` invocation** — terminate tagged-and-expired instances, delete orphan
   schedules, delete offline `burst-*` runner registrations. Idempotent; needs no memory of who
   launched what.
5. **AWS Budget alarm (~$15/mo)** — not a cleanup layer; the tripwire that a bug in all four
   costs days of pennies, not a month of silence. (GitHub itself GCs offline runner registrations
   after ~14 days; orphaned registrations are cosmetic, never billing.)

Every failure mode in the first-principles doc's table (CLI killed mid-launch, cloud-init hang,
kernel wedge, hung job, cancelled workflow, token expiry, spot reclaim, API errors during sweep,
user never runs burst again) is caught by at least one user-independent layer.

### Routing and coexistence with the home runner

Burst-eligible workflows declare `runs-on: [self-hosted, burst]`. The home runner carries
`[self-hosted, linux, x64, home, burst]` — it also accepts burst work, so one stray benchmark job
never strands when no fleet is up. Everyday CI targets `home`, which burst VMs don't carry, so the
fleet never steals the fast path. Labels only; runner groups are org-scoped and add nothing here.

### The image: prebaked, built by the tool itself

Boot-time install costs 5–10 min of exactly the latency this tool exists to remove. `burst bake`
launches one instance from stock Ubuntu/Debian, runs the version-controlled provisioning script
(toolchain, runner agent, browsers, X stack, the VM agent + units), optionally `cargo fetch &&
cargo build` against main for a warm `target/`, snapshots via CreateImage, keeps the last two AMIs,
terminates the builder — which is protected by the identical tag/kill-schedule layers. No Packer,
no Ansible.

**Bake cadence is a correctness floor, not just latency** (research catch the first-principles
design missed): GitHub stops dispatching jobs to runner agents ≳30 days old, and we run
`--disableupdate` (an ephemeral VM should not spend boot time self-updating). So `burst up` warns
when the AMI's baked agent is >21 days old, and the failure mode "fleet boots but receives
nothing" is pre-empted rather than debugged. Rebake monthly-ish or on warn.

**Cache warmth, phase 2:** sccache with an S3 backend shared by home runner and fleet keeps builds
warm *between* bakes and makes AMI staleness degrade gracefully. Deferred — measure fleet build
times with the baked-`target/` tier first; don't stand up the bucket until the pain is shown.

### Concurrency and instance economics

**N independent VMs, one runner each** — never one big box: GitHub parallelism is per-runner
anyway; browser benchmarks fight over displays/ports/CPU when co-tenanted; N common-size instances
launch more reliably than one rare 64-vCPU one; one failure costs 1/N. `burst up 4` is the primary
UX — the human just pushed the matrix and *is* the demand signal; `--auto` (size N from queued-job
count, capped at `max_fleet=8`) is the lazy path. No mid-flight rescaling: grow by running `up`
again, shrink is the idle timeout. **On-demand by default, `--spot` opt-in**: loop latency is the
stated priority, and a spot reclaim mid-benchmark taxes the loop to save single-digit dollars per
month; `--spot` is right for long parametric sweeps where a lost shard is cheap.

### Security posture

- **PAT** (fine-grained, single-repo, self-hosted-runners:write) lives on the dev machine only —
  never on any VM.
- **On the VM**: only the short-lived runner registration material and an instance profile scoped
  to nothing (until sccache lands: RW on one S3 prefix only). No SSH keys by default (`--ssh-key`
  for debug sessions). Security group: **zero inbound**; the runner long-polls outbound.
- Egress allow-listing rejected for now: GitHub's CIDRs churn, benchmarks need the real web, and
  the fork-approval gate carries the trust load. (Reserved-adjacent: if the egress-firewall design
  (D24/S6) later lands a posture, the fleet should inherit it — flagged, not decided here.)
- Worth saying out loud: the *home* runner — persistent, on the home LAN, state accumulating
  across jobs — is the scarier machine. The fleet is fresh-imaged, isolated, dead in hours.

## 3. The one adjudicated disagreement: registration mode (a call for James)

The two research reports strongly recommend GitHub's **JIT config** (`generate-jitconfig`): a
single-use, pre-scoped blob; the runner starts with `run.sh --jitconfig <blob>`, makes zero
registration API calls, and **no token capable of registering anything ever touches the VM**. The
first-principles design instead uses a classic **registration token** and deliberately
non-ephemeral runners.

The catch the researchers under-weighted: **JIT runners are ephemeral by construction — one job,
then gone.** Our lifecycle is "VM drains many jobs until idle." Reconciling JIT with that means
either one-VM-per-job (RunsOn's model — pays a boot per job; wrong for a 12-job matrix on 4 VMs)
or minting fresh JIT configs mid-life, which puts a minting credential on a machine that executes
CI code — strictly worse than the thing JIT was avoiding.

**Options:**
- **(a) Registration token + non-ephemeral (the lean).** Blast radius if a VM is compromised
  despite invariant 5: the token can register rogue runners against this one repo for ≤1h
  (rogue runners could receive later queued jobs). Bounded, single-repo, and gated behind
  "approved code went rogue."
- **(b) JIT + one-VM-per-job.** Cleanest credential story GitHub offers; costs ~90s boot per job
  and more launch API churn. Right answer if (a)'s token window ever feels wrong.
- **(c) JIT + CLI pre-mints K configs per VM.** Caps jobs-per-VM at K, configs expire in ~1h
  anyway; awkward middle, listed for completeness.

Lean: **(a)**, revisit toward (b) only if the threat model changes (e.g. the repo ever loosens the
fork-approval gate). The token choice is one function in the CLI either way — cheap to flip.

## 4. What we are explicitly NOT building

No webhook autoscaler; no always-on anything; no k8s; no Terraform/Ansible/Packer dependency; no
separate bootstrap/install step (`ensure_substrate()` above — the tool is its own installer); no
mid-flight rescaling; no multi-cloud *implementation* (a five-method
`Cloud` seam — `launch / terminate / list_tagged / arm_kill / bake` — keeps the door open; GCP's
native `max-run-duration` would even collapse layer 1 into the launch call, but a second backend is
a 300-line class written when actually wanted, not before); no per-job microVMs; no dashboard
(`burst status` prints text). The test applied to every exclusion: does its failure mode cost more
maintainer attention than its absence costs money or risk?

## 5. Rollout

1. **`burst bake` v1** (which exercises `ensure_substrate()` on the fresh account — there is no
   separate bootstrap step): provisioning script + first AMI; verify boot-to-registered <2 min.
2. **`burst up/down/status/sweep` v1**: launch against a synthetic queued matrix; then kill-test
   every cleanup layer deliberately (kill the CLI mid-launch, boot a broken AMI, hang a job,
   cancel a run mid-flight) and watch each layer catch its case — the layers are only real once
   each has been observed firing.
3. **First real burst**: a browser-benchmark matrix on `runs-on: [self-hosted, burst]`, measured
   against the serial baseline.

**Open calls for James:** (1) registration mode — §3, lean (a); (2) idle-timeout and TTL defaults
— proposed 15 min / 6 h; (3) instance type for benchmarks (lean: c7i.2xlarge-class, benchmarks may
want bare-metal-ish consistency — measure first); (4) sccache S3 tier now or after measuring
(lean: after).
