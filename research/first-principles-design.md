# `burst` — ephemeral cloud CI runners with guaranteed teardown

*Commissioned first-principles design (Fable, no research required). One of three inputs to
`../design-proposal.md`. Note: the consolidated proposal amends this design in places (JIT
adjudication, bake-cadence floor, zero-pre-setup substrate) — where they disagree, the proposal is
the current position.*

A design for a single command that rents parallelism by the hour and provably gives it back.

---

## 0. The one-paragraph design

`burst up N` is a ~800-line Python script that launches N tagged EC2 instances from a prebaked AMI, hands each a 1-hour GitHub runner registration token via user-data, and exits (or optionally stays to watch). Each VM is **self-managing**: it registers as a labeled self-hosted runner, works until it has been idle past a timeout, then runs `poweroff` — and because it was launched with `instance-initiated-shutdown-behavior=terminate`, poweroff **is** termination and billing stops. Cleanup is guaranteed by five independent layers, the deepest of which is a per-instance EventBridge Scheduler one-shot that calls `TerminateInstances` at launch+6h no matter what the VM, the script, or GitHub are doing. There is no daemon, no webhook receiver, no state file: **the cloud is the state, tags are the schema**, and every `burst` invocation begins by sweeping anything tagged as ours that has outlived its welcome.

**Invariants (the whole design in five lines):**

- **INVARIANT 1 — Scale to zero.** At idle, the only extant resources are: one AMI + its EBS snapshot, one S3 sccache bucket, one IAM role, one launch template. All are free or pennies. Nothing executes.
- **INVARIANT 2 — Tag or it doesn't exist.** Every instance carries `beep-burst=1` and `beep-burst-expires=<ISO8601>`. Anything so tagged past its expiry is fair game for termination by anyone, any time, without inspection.
- **INVARIANT 3 — Termination needs no cooperation.** No cleanup path requires the VM's OS, the GitHub API, or the launching script to be alive. The deepest layer is pure AWS control plane.
- **INVARIANT 4 — The VM never holds a credential that outlives it or exceeds it.** The only GitHub secret on a VM is a 1-hour registration token; the only IAM permission is RW to one S3 cache prefix.
- **INVARIANT 5 — Untrusted code never reaches a runner.** Fork PR workflows require explicit approval before any job runs (repo setting, non-negotiable), because a fork can edit `runs-on:` and labels are not a security boundary.

## 1. Architecture

Four components. Everything else is deliberately absent.

| Component | Where it lives | Exists at idle? | Why it exists |
|---|---|---|---|
| `burst` CLI (one Python file, in-repo) | Dev machine | As a file only | The only thing with the GitHub PAT and full EC2 rights. Launches, sweeps, reports. |
| VM-side agent (~120 lines bash + 3 systemd units) | Baked into the AMI | As bytes in a snapshot | Registers the runner, watches idleness, arms local dead-man timers, powers off. |
| The AMI (+ warm-cache payload) | EC2/EBS | Yes — the pennies | Boot-to-first-job in <90s. Boot-time install would add 5–10 min to exactly the loop we're trying to shorten. |
| Per-instance EventBridge one-shot kill schedule | AWS control plane | No — created at launch, self-deletes after firing | The dead-man's switch that works when everything else is dead. |

**Justified absences:** no autoscaler/webhook receiver (that's a daemon with a public endpoint, a cert, and an on-call rotation of one), no k8s / actions-runner-controller (a full-time platform for a part-time problem), no Terraform (a state file is a second source of truth that can disagree with the cloud; tags can't), no database (ditto — `describe-instances --filters tag:beep-burst` *is* the database).

**Why the CLI exits rather than supervising:** "script killed mid-run" is in the threat model, so nothing correctness-critical may live in the script's runtime. If the CLI can be killed safely at any instruction, it must not be a required participant after launch. `burst up` therefore does its work in an order where dying at any point leaves only resources that other layers will reap (see §8), then exits. `burst watch` is offered as an optional read-only tail for humans who like watching.

## 2. Lifecycle state machine — and why the VM is self-managing

```
            CLI drives                      VM drives                         AWS drives
┌─────────┐  RunInstances   ┌──────────┐  config.sh   ┌─────────┐   idle>T   ┌──────────┐
│ (none)  │────────────────▶│ BOOTING  │─────────────▶│ WORKING │───────────▶│ POWEROFF │──▶ TERMINATED
└─────────┘  + tag          └──────────┘  + svc start └─────────┘  poweroff  └──────────┘   (billing off)
      │       + kill-schedule     │                        │  ▲
      │                           │ no registration        │  │ job start/finish
      └── sweep expired (first)   │ within 10 min          │  │ (runner hooks touch
                                  ▼                        │  │  /run/last-activity)
                              POWEROFF ◀───────────────────┘
                                            hard 6h cap (systemd timer)
```

**Who drives what:**
- **CLI → BOOTING:** sweep first, then `RunInstances` (tagged, shutdown-behavior=terminate), then create the EventBridge kill schedule for the returned instance ID. Exit.
- **VM → WORKING:** cloud-init reads the registration token + labels from user-data, runs `config.sh --url … --token … --labels burst --unattended --disableupdate`, starts the runner as a service. Arms two local timers *before* starting the runner: a 10-minute "never registered" bootstrap deadline and a 6-hour absolute-lifetime `systemd-run --on-active=6h poweroff`.
- **VM → dead:** runner job hooks (`ACTIONS_RUNNER_HOOK_JOB_STARTED` / `_COMPLETED`) touch `/run/last-activity`; a 1-minute systemd timer checks `now − last_activity > IDLE_TIMEOUT && runner-not-busy` → `systemctl poweroff`. Poweroff = terminate (launch flag). **Note the elegance: self-termination requires zero IAM permissions.**
- **GitHub → registration GC:** the runner record goes offline at poweroff; the next `burst` sweep deletes offline `burst-*` runners via API, and GitHub itself auto-removes runners offline >14 days. Orphaned registrations are cosmetic, never billing.

**Self-managing vs. externally managed — the argument.** An external manager (CLI or reaper daemon) that decides when VMs die must (a) stay alive, (b) reach both AWS and GitHub, and (c) correctly infer remote VM state — three failure surfaces, one of which (a) directly violates scale-to-zero. The VM deciding its own death needs only local facts ("have I run a job lately?") and a syscall (`poweroff`). "Queue is empty" doesn't even need to be *detected* — an idle runner past its timeout is the queue-empty signal, observed for free, with no GitHub API call and no clock-skew races between an observer and the queue. External layers exist in this design, but strictly as **backstops for a hung VM**, not as the normal path. Self-managing, decisively.

## 3. Cleanup guarantees — five layers, ranked by reliability

Ranked from *most reliable* (fewest things that must be alive) down:

1. **EventBridge Scheduler one-shot → `TerminateInstances(id)` at launch+6h.** Pure AWS control plane. Survives: kernel hang, runner hang, GitHub outage, CLI death, user on vacation. Fails only with the EC2 API itself. Self-deletes after firing (`ActionAfterCompletion=DELETE`) — so it also satisfies scale-to-zero.
2. **`instance-initiated-shutdown-behavior=terminate`.** Turns every OS-level `poweroff` — idle watchdog, bootstrap deadline, 6h cap, even a panicking init — into full termination. A property of the instance, set before boot, unremovable by anything running on it short of root calling `ModifyInstanceAttribute` (which the instance role can't).
3. **On-VM systemd timers** (10-min bootstrap deadline; 6h absolute cap; 1-min idle check). Catch hung jobs, hung runner processes, boots that never registered. Fail only if the kernel itself wedges — which layer 1 covers.
4. **Sweep-on-every-invocation.** `burst` (any subcommand) first terminates every `beep-burst`-tagged instance past its `beep-burst-expires` tag, deletes stale kill-schedules, and deletes offline `burst-*` runner registrations. Because tags are the state, the sweep needs no memory of who launched what. Reliability limited only by "the user eventually runs burst again."
5. **The alarm bell, not a layer:** an AWS Budget alert at $15/month. Prevention is layers 1–4; this is the guarantee that a bug in all four costs days of pennies, not a month of silence.

Note what's *absent*: `--ephemeral` runner registration is deliberately **not** used, despite its attractive auto-deregister semantics — because ephemeral means one-job-per-registration, which means the VM must mint fresh tokens mid-life, which means a token-minting credential on a machine that executes CI code. That trade (see §7) is wrong. Registration cleanup is instead delegated to layer 4 + GitHub's own 14-day offline GC, both of which handle a *cosmetic* problem; the token-on-VM problem is a *security* one. Clean-up guarantees should be spent where the blast radius is.

## 4. GitHub integration

**Credentials, and where they live:**

| Secret | Scope | Lives | Blast radius if VM is compromised |
|---|---|---|---|
| Fine-grained PAT, single repo, `administration: write` only | mint/remove registration tokens, list runners | Dev machine only (env var / keychain). **Never on any VM.** | n/a — never leaves home |
| Registration token (minted by CLI per launch) | register a runner to this one repo; expires in 1h | User-data (readable via IMDS by anything on the VM) | Attacker can register rogue runners against this repo for ≤1h — they'd receive future queued jobs. Real but bounded; acceptable for a solo repo where only approved code runs (Invariant 5). Not acceptable would be an org-wide token or a PAT. |
| Instance profile | `s3:GetObject/PutObject` on `s3://…/sccache/*` only | The VM | Attacker can poison the compile cache. Mitigated by Invariant 5; sccache keys on compiler+source hashes, limiting practical poisoning. No EC2, no IAM, no other S3. |

**Routing via labels.** Burst-eligible workflows declare `runs-on: [self-hosted, burst]`. The home runner carries labels `[self-hosted, linux, x64, home, burst]` — it *also* accepts burst work, so a single stray benchmark job doesn't strand in the queue when no fleet is up. Everyday CI targets `[self-hosted, home]`, which burst VMs don't carry, so a fleet never steals the fast path's warm local caches. Two labels, no groups (runner groups are org-only; this is a personal repo).

**"Queue is empty"** is never queried in the steady state — idleness is the proxy (§2). The GitHub API is consulted in exactly two places: `burst up --auto` reads queued job counts to size N, and the sweep lists/deletes offline runners. Both run on the dev machine with the PAT; a GitHub outage degrades them to "use the explicit N flag" and "sweep next time" — never to leaked money.

**Fork PRs — the part that actually matters.** GitHub's own documentation warns against self-hosted runners on public repos, because a fork can rewrite the workflow file — including `runs-on:` — so **labels route jobs; they do not gate trust**. The mandatory mitigation is the repo setting **"Require approval for all outside collaborators"** on Actions. With it, no fork-authored workflow executes on any runner (home or burst) until the maintainer clicks approve, at which point they've read the diff. `burst` refuses to launch (hard error, `--i-understand-fork-risk` to override) if the repo's approval setting is anything weaker — checking a setting is cheap; a crypto miner with your S3 credentials is not.

## 5. The image

**Prebaked, aggressively.** Boot-time install (runner tarball, Rust toolchain, Chromium, Xvfb, fonts…) costs 5–10 minutes of exactly the latency this tool exists to remove, and adds a network-dependent failure mode to every single launch. The AMI bakes: pinned toolchain, runner binary (`--disableupdate` at config time), browsers, X stack, the VM agent, and the systemd units.

**Built by the tool itself:** `burst bake` launches one on-demand instance from stock Ubuntu, runs the same provisioning script that's version-controlled in-repo, runs `cargo fetch && cargo build --workspace` against current `main`, then `CreateImage`, waits, tags the AMI, deregisters the previous one (keep last two), terminates the builder. Same tags, same kill-schedule, same sweep — the bake instance is protected by the identical five layers. Run it manually when the base rots, or as a monthly scheduled GitHub Actions workflow on the home runner.

**Cache warmth — the two-tier play.** Rust CI latency is dominated by cold `target/` and cold registry.
- **Tier 1 (in the AMI):** the bake step's `~/.cargo` and a recent `target/` ship in the image. A benchmark-fleet build then compiles only the drift since the last bake — typically seconds-to-a-minute of delta, not a 10-minute cold build.
- **Tier 2 (continuous):** `sccache` with the S3 backend, shared by home runner and fleet. This keeps warmth *between* bakes and means AMI staleness degrades gracefully (more sccache hits) instead of sharply. The S3 bucket is the one standing resource beyond the AMI; storage cost is noise.

Bakes are triggered by pain, not schedule-worship: when boot-to-green drifts up (Cargo.lock churn), rebake.

## 6. Concurrency

**N independent VMs, one runner each. Not one big VM.** Three reasons: (1) GitHub Actions parallelism is per-runner — one machine needs N runner instances anyway, so "one big VM" saves nothing architecturally; (2) browser benchmarks are exactly the workload that fights over displays, ports, `/tmp`, and CPU cache when co-tenanted — isolation is correctness here, not luxury; (3) N small spot/on-demand instances of a common size (c7i.2xlarge-class) launch faster and more reliably than one rare 64-vCPU box, and a single instance failure costs 1/N of the fleet.

**Choosing N:** `burst up 4` is explicit and is the default UX — the human knows they just pushed a 12-job benchmark matrix. `burst up --auto` exists for the lazy path: count queued jobs whose labels ⊆ available burst labels, `N = min(ceil(queued / 1), max_fleet)` with `max_fleet` defaulting to 8 as a cost circuit-breaker. No feedback loop, no mid-flight rescaling — if the queue grows after launch, the human runs `burst up 2` again; idle VMs die on their own. Rescaling logic is an autoscaler wearing a trench coat; refuse it.

**Spot vs on-demand:** **on-demand by default, `--spot` as an opt-in.** The stated priority is loop latency over cost. A spot interruption mid-benchmark fails the job, GitHub does not auto-retry it, and the human notices twenty minutes later — that's the *loop* being taxed to save ~60% on an instance that lives two hours a few times a month (single-digit dollars). `--spot` is right for the long parametric sweeps where a lost shard is cheap to re-run; the flag makes the trade explicit rather than ambient.

## 7. Security posture

- **Trust model:** runners execute repo code; therefore the *repo's* approval gate (Invariant 5, §4) is the primary control, and everything on the VM is designed assuming that gate held — while still minimizing damage if it didn't.
- **On-VM secrets:** one 1-hour registration token (already spent by the time a job runs, though still IMDS-readable until expiry) and one cache-prefix-scoped instance profile. No PAT, no SSH authorized_keys by default (`--ssh-key` flag for debugging sessions only), no cross-service IAM.
- **Network:** default-VPC egress-open, ingress **none** (security group with zero inbound rules — the runner long-polls GitHub outbound; nothing ever needs to reach the VM). Egress lockdown to GitHub+crates.io+S3 CIDRs is tempting and rejected: GitHub's IP ranges churn, benchmark fleets need the actual web, and a solo maintainer debugging "why can't the runner connect" at 11pm is a real cost against a marginal gain *given the approval gate*.
- **Why the home runner is the scarier machine:** it's persistent, on the home LAN, and accumulates state across jobs. The burst VMs — fresh-imaged, isolated-VPC, dead in hours — are the *low*-risk half of this system. Worth saying out loud because intuition says the opposite.
- **Non-ephemeral runner, reused across jobs within one VM life:** accepted. Jobs within a two-hour fleet share a filesystem; all of them passed the same approval gate. The alternative (ephemeral + token minting on-VM) trades a hygiene concern for a credential-escalation concern — wrong direction (§3).

## 8. Failure-mode table

| Failure | Blast radius if uncaught | Caught by |
|---|---|---|
| CLI killed after `RunInstances`, before kill-schedule created | Instance without its deepest backstop | Layers 2+3 (OS timers, shutdown=terminate); tag sweep on next invocation. Launch order also puts tagging inside the `RunInstances` call itself (`TagSpecifications`), so there is no untagged window at all. |
| VM hangs at boot / cloud-init fails | Billing forever, no runner | 10-min bootstrap deadline → poweroff → terminate; 6h cap; EventBridge kill; sweep |
| Kernel wedges mid-job | OS timers dead | EventBridge one-shot terminate (layer 1) |
| Job hangs forever | Runner busy, idle check never fires | 6h on-VM cap; then EventBridge kill. GitHub marks the job failed when the runner vanishes — correct outcome for a hung job. |
| Workflow cancelled mid-job | None (runner returns to idle) | Idle timeout, normal path |
| GitHub API down at launch | CLI can't mint token → clean abort, nothing launched | Order of operations: token first, instances second |
| GitHub unreachable *from the VM* (partition) | Runner idle, can't receive work | Indistinguishable from empty queue — idle timeout fires. Correct by construction. |
| Registration token expires before boot completes | Runner never registers | 10-min bootstrap deadline |
| Spot reclaim (if `--spot`) | Job fails visibly in Actions UI | Human re-runs; instance terminates natively; no cleanup debt |
| AWS API errors during sweep | Stale resources persist one cycle | Sweep is idempotent and runs on *every* invocation; EventBridge kills are per-instance and independent |
| Orphaned runner registrations | Cosmetic clutter in repo settings | Sweep deletes offline `burst-*`; GitHub auto-GC at 14 days |
| User never runs `burst` again | Sweep layer inert | Layers 1–3 are all user-independent; budget alarm as tripwire |
| VM compromised by approved-but-malicious code | Cache poisoning; ≤1h rogue-runner registration | §7 scoping; approval gate is the real control |

## 9. Implementation sketch

**CLI: one Python file, boto3 + `urllib`/`requests`, in-repo (`ci/burst.py`), ~800 lines.**
- *Not bash:* five cleanup layers with correct error handling, JSON APIs on both sides, and a failure-ordering argument is beyond the honesty point of bash.
- *Not Go/Rust:* a tool edited twice a year by its only user wants zero build step and stack traces you can read; boto3's coverage of EventBridge Scheduler and EC2 is exactly the ergonomics needed. (A Rust dev's instinct says `cargo xtask burst`; resist — this tool must work even when the workspace doesn't compile, which is *precisely when you want more CI runners*.)
- **Portability seam:** one `Cloud` class with five methods — `launch(n, expires) -> ids`, `terminate(ids)`, `list_tagged() -> [(id, expires)]`, `arm_kill(id, at)`, `bake()`. AWS-only implementation today. Do **not** abstract further now: a second backend, if ever, is a 300-line class, and speculative abstraction is how solo-maintainer tools die. GCP/Azure notes: both have *native* instance TTLs (GCP `max-run-duration`, Azure via scheduled runbooks), which would let layer 1 collapse into the launch call — the seam accommodates that.

**VM agent: ~120 lines of bash + 3 systemd units, baked into the AMI**, versioned in-repo next to the provisioning script.

**Key flows (pseudocode):**

```
burst up N:
  sweep()                                   # always first — cleanup is rent paid on entry
  assert repo.fork_approval == "all_outside_collaborators" or --i-understand-fork-risk
  token = github.mint_registration_token()  # PAT never leaves this machine
  ids = ec2.run_instances(
          n=N, ami=latest_tagged_ami, type=cfg.type,
          user_data=render(token, labels="burst", idle=cfg.idle_timeout),
          shutdown_behavior="terminate",
          tags={beep-burst: 1, beep-burst-expires: now+6h})   # tagged atomically
  for id in ids: scheduler.create_one_shot(now+6h, TerminateInstances(id), self_delete=True)
  print(ids); exit 0

sweep():
  for (id, expires) in ec2.list_tagged():   # tags ARE the state
      if now > expires: ec2.terminate(id)
  scheduler.delete_orphans(prefix="beep-burst-")
  for r in github.list_runners(prefix="burst-"):
      if r.offline: github.delete_runner(r)

VM boot (cloud-init):
  systemd-run --on-active=6h  poweroff              # absolute cap, armed FIRST
  systemd-run --on-active=10m check-registered-or-poweroff
  ./config.sh --unattended --disableupdate --labels burst --token $TOKEN
  systemctl start runner idle-watchdog.timer        # hooks touch /run/last-activity

idle-watchdog (every 60s):
  busy && exit
  (now - mtime /run/last-activity) > IDLE_TIMEOUT && poweroff   # poweroff == terminate
```

**Subcommands:** `up [N|--auto] [--spot]`, `status` (tagged instances + runner list, plain text), `down` (terminate all tagged now), `sweep`, `bake`, `watch`. That's the whole surface.

## 10. What I would NOT build

- **A webhook-driven autoscaler** (ARC, philips-labs terraform-aws-github-runner). The gold standard for teams; for one human it means a public endpoint, secret rotation, and a distributed system to debug — to save typing `burst up 4` at the moment you already know you need it. The human *is* the demand signal; use them.
- **Any always-on component** — daemon, Lambda poller, warm pool, "standby" instance. Each violates Invariant 1 and each is a thing that can be broken while you're not looking.
- **Mid-flight rescaling / queue-tracking feedback loops.** Launch is idempotent-ish (run `up` again); shrink is automatic (idle timeout). A control loop here is complexity with no customer.
- **Terraform/CloudFormation for the runtime resources.** IaC state that can disagree with reality is the opposite of "tags are the schema." (The *one-time* setup — role, bucket, budget — can be a documented 20-line bootstrap script; it runs twice a decade.)
- **Multi-cloud support now.** Keep the five-method seam, ship AWS.
- **Per-job microVMs (Firecracker), runner groups, JIT/ephemeral token plumbing, egress allow-listing.** Each is defensible at org scale; at solo scale each buys marginal hygiene at real ongoing cost, and §7's approval gate already carries the trust load.
- **A dashboard.** `burst status` prints text; CloudWatch exists for the once-a-year archaeology.

The test for every excluded item was the same: *does its failure mode cost more attention than its absence costs money or risk?* For a solo maintainer, attention is the scarce resource this tool exists to protect — the design spends AWS's control plane and GitHub's built-in GCs freely, and the maintainer's future evenings never.
