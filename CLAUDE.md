# CLAUDE.md — burst-actions project guide

> **Fresh session: read this file, then `design-proposal.md` in full.** Those two reconstruct the
> project. `research/` is background only — the proposal supersedes it wherever they disagree.

## What this project is

A single Rust binary, `burst`, that turns cloud VMs into ephemeral GitHub Actions self-hosted
runners on demand: `burst up N` (or `--auto`) launches N instances from a prebaked AMI, each
registers via a single-use JIT config, runs **exactly one job**, and self-terminates; the command
watches until the fleet is gone and everything is cleaned up. Scale-to-zero, guaranteed cleanup,
zero AWS pre-setup, genuinely open source. The full design — invariants, lifecycle, cleanup
layers, image cache, decision log — is `design-proposal.md`. This file is about how to *work* on
it.

The user is James Shkolnik (jshkolnik@gmail.com). The motivating deployment: his one fast home
runner carries everyday CI; burst covers heavy-parallelizable pipelines (e.g. a browser-benchmark
matrix that is N independent 20-minute jobs). Design for that shape of user — a solo maintainer
with AWS + GitHub credentials and no patience for standing infrastructure — not for a platform
team.

## Status and your job

Design **approved 2026-08-08**; nothing built. Your job is the rollout plan (`design-proposal.md`
§7): scaffold the crate, then `burst bake` v1 + `ensure_substrate()`, then
`up`/`down`/`status`/`sweep` v1 with deliberate kill-testing of every cleanup layer, then a first
real burst measured against a serial baseline.

**The decision log (§6) is settled — do not re-litigate it.** Ten decisions, all James's. If
implementation reveals a decision is genuinely wrong, that's a finding to bring to him with
evidence and options, never a thing to silently redesign around. §8's defaults are the opposite:
**implement as written without asking**; they're expected to be tuned after first contact.

## Hard rails

- **Real money.** This tool creates EC2 instances, AMIs, snapshots, IAM roles, and schedules in a
  real AWS account. Every instance you create — including by-hand experiments and half-built code
  paths — carries the three `burst-*` tags and an armed kill schedule *from launch*, exactly as
  invariant 2 demands; the discipline applies to your debugging instances before the tool
  enforces it for anyone. End every working session by running the sweep (or its manual
  equivalent: list instances tagged `burst=1`, verify none should be alive) and say in your
  report that you did. Use the smallest instance type that exercises the code path when testing;
  the configured type is for real workloads.
- **Credentials.** The GitHub PAT lives on the invoking machine only and must never reach a VM in
  any form; a VM gets exactly one single-use JIT config (invariant 4). If AWS or GitHub
  credentials are missing from your environment, ask James — never work around it, never bake a
  credential into anything committed.
- **Never** weaken the fork-approval preflight (invariant 5) or make cleanup depend on the
  watcher being alive (invariant 3) for convenience during development. Those two properties are
  the product.

## Working norms (match these — they're James's)

- **Verify by running.** A cleanup layer is only real once you have watched it fire: kill the CLI
  mid-launch and watch tags+schedules mop up; boot a broken AMI and watch the bootstrap deadline
  poweroff; hang a job and watch the TTL kill. Rollout step 2 exists for this. The same applies
  in miniature to every feature: "the test passes" is not "I watched the behavior."
- **Prove a guard test's teeth by regressing the production code**, not by trusting that it was
  red once during development.
- **Fail loud, never degrade.** An expired PAT, a missing default VPC, a quota cap below the
  request — each gets a clear, actionable error (or explicit warning), never a silent partial
  result. When an error names a remedy, verify the remedy actually works before shipping the
  message.
- **Flag verified vs. inferred.** James corrects under-hedging as readily as overclaiming. "I ran
  it and saw X" and "the docs say X" are different sentences — write them differently.
- **Critical engagement over agreement.** He wants pushback, surfaced tensions, and options with
  a stated lean on open calls — not validation. Security thinking in blast-radius/least-privilege
  terms lands well; name the tradeoff.
- **Commit as you go**, one verified change per commit, honest messages (a failing test is
  reported failing). Delegate mechanical execution to subagents freely, but verify every
  load-bearing claim yourself before reporting it.

## Implementation guardrails from the design

- Standalone crate, own repo, **never a workspace member of any consumer project**. Keep a
  prebuilt binary available once one exists: "CI is broken" must never block launching CI
  runners.
- Wire everything to the five-method `Cloud` trait seam (`launch / terminate / list_tagged /
  arm_kill / bake`) but build only the AWS backend.
- Naming: binary and crate `burst`; AWS resources `burst-*`; tags `burst=1`,
  `burst-repo=<owner/repo>`, `burst-expires=<ISO8601>`.
- Known wrinkles already researched (details in §5): IAM eventual consistency on fresh accounts
  (retry the specific invalid-role error); `--auto`'s label-filtered queue count is one API call
  per queued run, not one call total.
- Reference reading, lessons not code: **testflows/testflows-github-hetzner-runners** (the
  single-process one-VM-per-job watcher shape) and **CloudSnorkel/cdk-github-runners** (AWS
  edge-case history: launch-time tag races, instances dying before the agent installs). Both
  Apache-2.0; we import ideas, not source.

## Reserved for James (do not auto-decide)

Changing anything in the §6 decision log; adding any standing infrastructure (webhook receivers,
daemons, Lambdas — invariant 1 is a principle, not a proxy); egress restriction posture for fleet
VMs (§ Security flags it as future work); adding a second cloud backend; anything that would put
a credential on a VM beyond the JIT config. Frame such items as options + a lean.

## Housekeeping

- This directory is a local git repo (identity `shkolnik-beep` configured repo-locally); there is
  no remote yet — creating one (likely GitHub, given it's an OSS project) is worth raising with
  James early, along with the license choice (the design requires the tool itself be genuinely
  open source; Apache-2.0 or MIT are the natural candidates — his call).
- Keep this CLAUDE.md current as the project grows a real structure (crate layout, build/test
  commands, CI). Replace this Housekeeping section with real onboarding (build commands, test
  invocations, environment notes) as soon as those exist.
