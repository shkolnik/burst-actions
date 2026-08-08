# burst — implementation phases

Decomposition of `design-proposal.md` into four sequential phases. Each phase ends at a
verification gate testable against the compiled tool; a phase is done when its gate is
demonstrated, not when its code exists. **Detailed bite-sized task plans are written per phase at
phase start** (in `docs/plans/`), so task-level detail always reflects what earlier phases
actually built.

Why four: the system has three natural module seams — pure-local machinery (no network), the
AWS/image side, and the fleet-lifecycle orchestration — plus rollout step 3 (real-workload
measurement), which produces evidence, not modules. Fewer phases would put cloud credentials in
front of work that doesn't need them; more would cut through the `up` command's core loop, which
is the one place the pieces must be reasoned about together.

| Phase | Name | Verifies | Needs |
|---|---|---|---|
| 0 | Scaffolding | Crate builds/lints/formats clean; LSP works; dependabot+CI committed | Rust toolchain only |
| 1 | Local core & contracts | Lock/state/config/key behavior on the compiled binary, offline | Rust toolchain only |
| 2 | Substrate & image (rollout §7.1) | Fresh account → baked AMI; boot-to-registered < 2 min | AWS creds + GitHub PAT |
| 3 | Fleet lifecycle & kill-testing (§7.2) | Every cleanup layer observed firing | Same + synthetic CI matrix |
| 4 | First real burst & tuning (§7.3) | Wall-clock win vs serial baseline; §8 defaults tuned on evidence | A real benchmark matrix |

## Phase 0 — Scaffolding

Tooling on rails before feature work: cargo scaffold (edition 2024, Rust ≥ 1.89,
`AGPL-3.0-only`, all phase-1 deps declared), lint policy in `[lints]`
(forbid unsafe, warn on wildcard enum arms / dbg / todo), rustfmt + toolchain pin,
rust-analyzer for the editor/agent LSP, dependabot (cargo + github-actions ecosystems) and a
fmt/clippy/test CI workflow — both inert until the remote exists, live at first push.

**Gate**: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt
--check` clean on the committed tree; `rust-analyzer` present; YAML parses.

## Phase 1 — Local core & contracts

Everything that needs no cloud and no credentials; the parts later phases build against.

**Scope**
- Crate scaffold (edition 2024, `license = "AGPL-3.0-only"`), CI-less for now; binary `burst`.
- Full CLI surface parsed (`up [N|--auto] [--spot] [--yes] [--ssh-key]`, `bake`, `status`,
  `down`, `sweep`); unimplemented subcommands fail loud with "not yet implemented", never
  silently no-op.
- Config: `[burst]` TOML block + working defaults (§8 values as written); required-input
  validation (repo, credentials presence) with actionable errors.
- Statefile + lock: `~/.local/state/burst/<owner>-<repo>/` flock lockfile, JSON statefile with
  write-then-rename, abandoned-run detection (statefile present + lock acquirable), adoption
  reconciliation logic (as pure functions over a `Cloud` view).
- Schemas as types: tag triple (`burst-actions=1` / `burst-actions-repo` / `burst-actions-expires`), image-cache key
  (hash of provisioning script ∥ base image ID ∥ arch ∥ agent version), instance lifecycle and
  cleanup-outcome enums, error taxonomy. Closed sets are enums matched exhaustively.
- `Cloud` trait (`launch / terminate / list_tagged / arm_kill / bake`) + in-memory fake backend
  for tests. The fake is a test artifact only — no dry-run flag in the shipped binary.

**Out**: any AWS or GitHub call; the VM payload.

**Gate** (integration tests against the compiled binary, plus unit tests):
- Second invocation while lock held fails fast; killed process's statefile is detected as
  abandoned by the next.
- Statefile survives a kill between write and rename (reader sees old-or-new, never torn).
- Image key is stable across runs and changes when any input changes.
- Adoption reconciliation: file-not-cloud entries dropped, cloud-not-file instances adopted
  (against the fake backend).
- Guard tests proven by regressing production code once (per working norms).

## Phase 2 — Substrate & image (rollout §7.1)

First real AWS contact: the idempotent substrate and the bake path, plus the thin GitHub slice
needed to close this phase's stated gate.

**Scope**
- AWS `Cloud` backend: `RunInstances` with atomic `TagSpecifications`,
  `instance-initiated-shutdown-behavior=terminate`, IMDSv2 required; `terminate`; `list_tagged`;
  EventBridge Scheduler one-shot `arm_kill` (`ActionAfterCompletion=DELETE`).
- `ensure_substrate()`: get-or-create both IAM roles, security group (zero inbound), opt-in
  budget alarm, under deterministic `burst-actions-*` names; the IAM eventual-consistency retry (§5);
  fail-loud on missing default VPC (§8.7).
- VM payload authored & baked: provisioning script (pinned Ubuntu LTS base per §8.6, toolchain,
  runner agent `--disableupdate`, browsers/X, VM agent), systemd units for the three on-VM
  timers (bootstrap deadline, never-assigned idle, hard TTL).
- `burst bake` end-to-end: tagged+kill-armed builder → provision → `CreateImage` → stamp key
  tag → terminate builder → delete superseded AMI+snapshot. Cache-hit check in `up`'s path.
- GitHub slice: PAT auth + `generate-jitconfig` minting only — pulled forward from phase 3
  because this phase's gate (boot-to-registered) is unmeasurable without it, and it front-loads
  the riskiest external integration.

**Out**: fork-approval preflight, `--auto`, the watcher, sweep, adoption against real cloud.

**Gate** (on a fresh account, smallest viable instance type):
- `burst bake` on an empty account creates everything and produces a tagged AMI; re-run is a
  no-op (cache hit); config edit → rebake and superseded-image deletion. Verified by AWS
  listing, not by exit code.
- One manually launched VM from the AMI with a minted JIT config reaches "registered" on GitHub
  in < 2 min from `RunInstances` — measured, not asserted.
- Bootstrap deadline watched firing once: boot the AMI with a garbage JIT config, observe
  poweroff-as-termination at the 10-min deadline.
- Builder interrupted mid-bake (kill the CLI) leaves only a tagged, kill-scheduled instance
  that the schedule then reaps — watched.
- Every instance this phase creates carries the tag triple + armed schedule from launch; session
  ends with a sweep-equivalent listing showing zero live instances.

## Phase 3 — Fleet lifecycle & kill-testing (rollout §7.2)

The product: `up`'s full loop and the proof that cleanup is guaranteed, not best-effort.

**Scope**
- Remaining GitHub client: fork-approval preflight (hard error, invariant 5), `--auto`
  label-filtered queued-job count (one call per queued run, §5), never-connected registration
  cleanup.
- `burst up` complete per §3 lifecycle: lock/adopt → substrate → sweep-on-entry → preflight →
  AMI ensure → mint N → launch N → arm N one-shots → statefile → watch. Ctrl-C detaches;
  SIGKILL leaks nothing; quota check warns before capping.
- `burst down` (tag-verified terminate of this repo's fleet), `status` (cloud-truth text),
  `sweep` (expired instances, orphan schedules, dead registrations — idempotent).
- Adoption/resume against real cloud; cross-host advisory prompt (`--yes`).

**Out**: real workloads; any default-tuning.

**Gate** — the kill-test matrix, each row *watched*, against a synthetic queued matrix of
sleep-jobs on `runs-on: [self-hosted, burst]`:
- CLI SIGKILLed mid-launch → fleet finishes and self-terminates; next invocation adopts residue
  (layers 2/3/4).
- Broken AMI boot → bootstrap-deadline poweroff (layer 3; already seen in phase 2, re-verified
  through the full `up` path).
- Hung job → on-VM TTL cap, and separately a wedged VM → EventBridge kill (layers 3 then 1;
  layer 1 verified with on-VM timers disabled so it demonstrably acts alone).
- Cancelled workflow → jobless runner → idle-timeout poweroff (layer 3).
- `sweep` reaps an artificially expired instance and its orphan schedule (layer 4).
- Fork-approval preflight: weaken the repo setting, `burst up` hard-errors.
- Concurrency: second `up` fails fast; `up` during a live fleet from "another host" (simulated
  by hiding the statefile) triggers the advisory prompt.

## Phase 4 — First real burst & tuning (rollout §7.3)

**Scope**: run the motivating browser-benchmark matrix through `burst up --auto` against the
serial home-runner baseline; record wall-clock, per-phase latency (bake, boot-to-registered,
drain), and cost. Tune §8 defaults where evidence disagrees (instance type, volume, timeouts) —
findings with data go to James; defaults change only on measurement. Produce the sccache
go/no-go input (§3 phase 2 tier). Ship a prebuilt binary; replace CLAUDE.md housekeeping with
real build/test/run onboarding.

**Gate**: measured wall-clock improvement over the serial baseline on the real matrix, with the
cost figure alongside; a written findings note (what §8 defaults held, what changed, why).

## Proposed crate layout

Locked in phase 1, adjusted freely until then:

```
src/main.rs            CLI (clap), command dispatch
src/config.rs          TOML + defaults + validation
src/state.rs           lockfile, statefile, adoption logic
src/schema.rs          tags, image key, lifecycle/error enums
src/cloud/mod.rs       the five-method Cloud trait
src/cloud/aws.rs       the only real backend
src/cloud/fake.rs      in-memory test backend (cfg(test) or test-support feature)
src/github.rs          PAT auth, JIT mint, preflight, queue count
src/commands/*.rs      up, bake, down, status, sweep
vm/                    provisioning script, systemd units, runner wrapper (baked into AMI)
tests/                 integration tests against the compiled binary
```
