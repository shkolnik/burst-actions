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
  terms lands well; name the tradeoff. When he offers "a suggestion," that is a request to
  *evaluate* it — including telling him it's wrong — not an instruction phrased politely.
- **Naming is behaviour design.** A flag, tag, subcommand, or error name shapes what users do
  with it; treat renames and new names as design decisions, not cosmetics.
- **Frugal words.** In docs, CLI output, and reports, words that don't add meaning are wasted
  cost (agents pay per token to read them). Lead with the answer; organize for progressive
  disclosure — summary first, detail where the reader who needs it will look.
- **Commit as you go**, one verified change per commit, honest messages (a failing test is
  reported failing). Delegate mechanical execution to subagents freely, but verify every
  load-bearing claim yourself before reporting it.
- **Debug root-cause-first.** No fix attempts before you can state the root cause and reproduce
  it; one hypothesis, smallest possible test, one change at a time. Three failed fixes means the
  design is wrong — stop and bring it to James rather than trying a fourth.

## Engineering philosophy (generalized from James's other projects — apply here)

- **Make the compiler the reviewer.** An agent-built codebase is read a few files at a time; the
  compiler reads all of them at once. A closed set of alternatives (instance lifecycle states,
  cleanup outcomes, error kinds) is a Rust `enum` matched exhaustively — never a bare `String`
  compared with `==`. Adding a variant must *break the build* at every site that has to change,
  not leave stale branches silently taking the `else`.
- **Reversibility sets the burden of proof.** An internal enum is an afternoon to reverse;
  anything users script against once released — CLI flag names, tag schema, statefile format,
  exit codes — is close to forever. Under uncertainty, take the reversible option and revisit
  when real demand appears. Prerelease corollary: with no users yet, prefer the cleaner schema
  over compatibility with an on-disk format nobody depends on.
- **A finding is a sample, not the defect — fix the class.** A bug names one site because that's
  where attention landed. A fix is done when you can answer "what invariant did I restore, and
  where else must it hold?" — and the strongest fix makes the bad state unrepresentable rather
  than enumerating its instances. What you find but don't fix, you record.
- **One spelling per message.** Output is read by humans, scripts, *and* agents in CI logs; two
  spellings of one condition silently degrade the agent reader with no error anywhere. Structure
  the producer (one authoring site per message/error) and keep messages stable once shipped.
- **Wanting a long comment is evidence about the code.** Simplify first — a better name, a
  smaller function, a type. A comment earns its place caching a slow-changing *external* fact at
  the point of need (an AWS API quirk, IAM propagation behavior — exactly §5's wrinkles); it
  never narrates history or demonstrates that you did the research. Evidence of verification
  goes in the commit message or a test that fails when the fact stops being true.
- **Prove ownership before destroy.** Never terminate or delete by name-pattern or broad
  listing; every destructive operation re-verifies the `burst-*` tag identity of the specific
  resource immediately before acting. The failure mode — killing something in the account that
  isn't ours — is irreversible, so this holds even in throwaway debugging scripts.
- **The environment is an undeclared dependency.** Probe for what you need (AWS credentials,
  region, default VPC, quota) and either work fully or refuse with the reason — never degrade
  into partial behavior because something was absent.

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
  no remote yet — James will set up the GitHub server side; push when it exists. License is
  decided: **AGPL-3.0-only** (James, 2026-08-08 — maximal-control OSI license; he is sole
  copyright holder and can relicense later; revisit CLA/DCO if outside contributors appear).
- Implementation follows `implementation-phases.md` (four phases, each with a verification
  gate); per-phase task plans live in `docs/plans/`. Execution model: subagent-driven
  development — Sonnet/Opus subagents implement tasks by clarity/complexity, Fable leads and
  reviews whole-branch / high-importance gates.
- Keep this CLAUDE.md current as the project grows a real structure (crate layout, build/test
  commands, CI). Replace this Housekeeping section with real onboarding (build commands, test
  invocations, environment notes) as soon as those exist.
