# OSS-only landscape sweep (commissioned 2026-08-08)

> Commissioned after James made "genuinely open source — find it or write it" a hard requirement
> (which ruled out RunsOn on licensing). Question: does any genuinely-OSS tool for bursting GH
> Actions runners on AWS EC2 exist that we missed? Two bars: A = adoptable, B = inspiration for
> the tool we write. Outcome: conclusion "no adoptable OSS tool" holds; CloudSnorkel/
> cdk-github-runners is the closest (rejected — always-armed webhook autoscaler, CDK model, no
> documented VM-independent kill switch) and is the top Bar-B reference. §2 of
> `design-proposal.md` was amended accordingly. Preserved verbatim below.

---

## 1. Search coverage

Web searches (WebSearch) covering: general "ephemeral GitHub Actions EC2 scale-to-zero 2026," Firecracker/microVM runner projects (fireactions and adjacent), GitHub topics (`ephemeral-runner`, `self-hosted-runner`, `github-actions-runner`, `github-runners`), Rust/CLI/JIT-runner angles, awesome-lists (`jonico/awesome-runners`, `neysofu/awesome-github-actions-runners`), Hetzner-style portable-design projects (testflows, louisgundelwein), EventBridge/Lambda kill-switch patterns, and HN/Reddit/Lobsters sentiment searches for "no kubernetes" AWS runner recommendations. Cross-checked candidate repos directly via `gh api` (stars, license, push dates, contributor counts, commit messages, open issues) rather than trusting summaries alone. No paywalled/closed tools (WarpBuild, BuildJet, Depot, Namespace) were evaluated in depth since they fail the OSI-license hard requirement on their face.

## 2. Candidates table

| Name | What it is | License | Last activity | Verdict | One-line reason |
|---|---|---|---|---|---|
| **CloudSnorkel/cdk-github-runners** | AWS CDK construct: Lambda+Step-Functions control plane provisioning ephemeral EC2/ECS/Fargate/CodeBuild/Lambda runners | Apache-2.0 | commits within days (checked 2026-08-08); 20 contributors, 399★/51 forks | **Bar A — strongest candidate found, not previously assessed** | Genuinely serverless (no standing compute), scale-to-zero, no k8s, EC2-native, actively maintained; caveat is CDK/CloudFormation deploy model, not a CLI |
| **hostinger/fireactions** | Firecracker-microVM orchestrator for self-hosted GitHub runners | Apache-2.0 | 174★, pushed 2026-07-20 | **Bar B** | Standing server+agent daemon (pool-based, not scale-to-zero) and explicitly bare-metal/BYOM — no documented AWS EC2 support; still worth reading for microVM lifecycle handling |
| **cisco-open/forge (ForgeMT)** | Multi-tenant platform for EC2 + ARC/k8s GitHub runner lanes, built inside Cisco | Apache-2.0 | 211★, pushed 2026-08-07, 568 commits | **Bar B, not A** | Explicitly a **standing control plane** for platform teams managing many tenants — wrong shape for a solo maintainer wanting nothing to babysit; EC2 lane design/tenant isolation worth skimming |
| **omsf/gha-runner** (+ `start-aws-gha-runner`) | Python lib/action, workflow-step start/stop EC2 runner, JIT config, jitconfig-based | MIT | 5★, pushed 2026-08-05 | Neither | Same category as already-known machulav/ec2-github-runner (step-based start/stop, cleanup rides a stop job); too small/early to be A, doesn't add new design ideas over the reference implementations already logged |
| **Open-Athena/ec2-gha** | Similar workflow-step EC2 runner action | MIT | 1★, pushed 2026-07-10 | Neither | Very early, single-digit adoption, same pattern already covered |
| **drakon64/github-actions-runner-aws** | Same category | — | — | Neither | Same pattern, not independently notable |
| **testflows/testflows-github-hetzner-runners** | Mature, actively maintained Python service: one throwaway VM per CI job, auto power-off + delete, no Lambda/webhooks needed, single-process operational model | Apache-2.0 | active | **Bar B (strong)** | No AWS support (Hetzner API only) so fails the hard requirement, but its "no standing control plane needed, simple single-service daemon polls GH API directly" architecture is a legitimate design reference for operational simplicity |
| **louisgundelwein/runner-autoscaler** | "One throwaway VM per CI job" autoscaler | — | — | **Bar B (minor)** | Hetzner-only; same one-VM-per-job philosophy, smaller/less proven than testflows' project |
| **GitHub Actions Runner Scale Set client (Go)** | Official Go module for building custom ARC-style autoscalers | MIT (GitHub) | active, 2026 updates | Neither | Purpose-built for the ARC/k8s scale-set model — same k8s dependency already ruling out ARC |
| Rust-native EC2/GH-runner orchestrator | — | — | — | **None found** | Searched crates.io + GitHub for a Rust tool matching this niche; nothing exists — confirms the gap our planned Rust tool would fill |

## 3. Detail on notables

**CloudSnorkel/cdk-github-runners — the one finding that could change the conclusion, deserves a closer look before ruling out "buy vs build."**
- Architecture: GitHub webhook → Lambda → Step Functions state machine → provisions an EC2 instance (or ECS/Fargate/CodeBuild/Lambda) per job, JIT-registered, terminated after the job. Nothing runs when idle — pure pay-per-invocation AWS primitives (Lambda, Step Functions, EventBridge for the webhook path). This satisfies the "no small always-on control plane" requirement about as cleanly as is possible on AWS.
- No Kubernetes anywhere in the EC2/ECS/Fargate/CodeBuild paths (ARC is only mentioned as an unrelated external alternative).
- Cleanup: retry logic and Step Functions state-machine timeouts govern failed launches (documented: "if there are any issues starting the runner... the provider will keep retrying for 24 hours"); the README does **not** explicitly describe an EventBridge one-shot kill-switch/sweep of the kind planned for our tool — this is the one area worth independently verifying (i.e., does an instance that starts fine but then hangs/crashes mid-job actually get force-terminated, or does it rely on the Step Function's own execution timeout only?). Recent commit history (checked live) shows exactly this class of hardening happening in real time — e.g. commit `dbb63c8` (2026-07-28) adds launch-time EC2 tags specifically because "the instance may terminate before the agent is installed" for short-lived ephemeral runners, showing the maintainer is actively fighting the same edge cases our design targets.
- Supports EC2 (unlimited runtime, x86_64/ARM64, GPU), and separately ECS/Fargate/CodeBuild/Lambda if ever wanted.
- Bus factor: 20 contributors, but commit volume is dominated by one maintainer (Eugene/CloudSnorkel handle) plus a "projen" bot doing dependency bumps — moderate, not GARM-style single-person risk but not a large team either.
- Operational shape: this is a **CDK app**, not a CLI — you write TypeScript/Python/Java/Go/.NET CDK code instantiating `GitHubRunners` and deploy via CloudFormation. For a solo maintainer that's arguably fine (declarative, no servers to SSH into) but it is a materially different operational model than a single Rust binary + tags + EventBridge sweep, and pulls in CDK/CloudFormation as a dependency chain. Multi-cloud portability: none — deliberately AWS-native (fine per your requirements).
- **This project was not on the already-assessed list and clears enough of the hard requirements (genuine Apache-2.0 OSS, EC2-native, zero standing infra, no k8s, actively maintained) that it deserves an explicit accept/reject decision rather than being silently grouped with the ruled-out Lambda-control-plane projects** (it differs from terraform-aws-github-runner/github-aws-runners in that there truly is no persistent webhook-receiving Lambda sitting around waiting — same-shaped compute, but the framing in the prior "standing control plane" rejection may not automatically apply here; worth a maintainer's explicit read of its Step Functions state machine before concluding either way).

**hostinger/fireactions — worth reading for the microVM angle, not adoptable.**
Firecracker jailer + control-plane/agent split, protobuf-based server↔agent protocol, pool-based warm-runner management, SSH-into-VM debugging support. If the project ever explores Firecracker microVMs instead of full EC2 instances as a cost/speed optimization, this codebase (Go, v2.0.0, actively iterated per its issue tracker) is the most relevant prior art found. Ruled out for adoption because it targets bare metal only (no EC2/cloud provider integration in its docs) and runs a standing pool-management daemon rather than scaling to true zero.

**cisco-open/forge (ForgeMT) — good tenant/IAM design reading, wrong operational shape.**
Real production pedigree (built inside Cisco's Security Business Group, OpenSSF Scorecard + Best Practices badges, 568 commits, active as of the last week). But it is architecturally a **standing control plane** serving many tenant repos — the antithesis of "nothing standing that must be babysat" for a solo maintainer. Worth skimming for how it does tenant-scoped IAM roles and AMI/lane separation if useful patterns are needed, but not a candidate to run as-is.

## 4. Bottom line

**The "no good genuinely-open-source tool for this exact use case" conclusion mostly holds, with one caveat that needs a deliberate look rather than a silent pass.**

- Nothing found is a drop-in replacement for the planned Rust tool (JIT+ephemeral one-VM-per-job, prebaked AMI cache, EventBridge kill-switches, tag-driven sweep, zero standing infra, single-person operability). No Rust-native tool of this shape exists anywhere searched.
- The Firecracker (fireactions) and standing-control-plane (ForgeMT) projects both fail hard requirements outright (no EC2 support / no scale-to-zero respectively) and land squarely in Bar B — useful design reading, not adoptable.
- The one project that plausibly clears Bar A and was **not** on the previously-assessed list is **CloudSnorkel/cdk-github-runners**: genuinely Apache-2.0 open source (full server + provisioning logic included, no open-core split), truly serverless/scale-to-zero, EC2-native, no Kubernetes, actively maintained with real bus factor. It differs from the previously-ruled-out Lambda-control-plane projects (terraform-aws-github-runner) in that it has no persistent webhook receiver sitting idle — Lambda + Step Functions only run when triggered. Its main weaknesses relative to the stated design goals are (a) CDK/CloudFormation as the deployment/operational model rather than a lean CLI, and (b) an unverified answer to "does a crashed/hung job's EC2 instance definitely get killed" — the README doesn't show an EventBridge-style unconditional sweep the way your planned design does. **Recommend a short, explicit spike (read the Step Functions state machine + test a deliberately-killed runner) before finalizing "we must build our own" — if its cleanup guarantee turns out as robust as the rest of the project looks, this is the closest thing to "good OSS tool" this search turned up, and the build-vs-adopt call should be made with that project named, not left implicit.**

---

## Post-sweep verification (main thread, 2026-08-08)

The cdk-github-runners README was independently re-fetched and confirmed the sweep's four
load-bearing characterizations: (1) trigger is a GitHub webhook, always armed — no on-demand
"launch N runners" command exists; (2) deployment is a user-authored CDK app deployed via
`cdk deploy`; (3) no itemized idle-cost or standing-resource discussion beyond "you don't pay
unless a job is running"; (4) cleanup documentation covers failed *starts* (24 h retry;
GitHub cancels unassigned jobs after 24 h) with no documented VM-independent kill switch for a
hung or crashed runner.
