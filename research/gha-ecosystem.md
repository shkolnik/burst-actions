# GitHub Actions Self-Hosted Runner Burst-Capacity Research

*Commissioned research report (Sonnet, web research). One of three inputs to `../design-proposal.md`.*

## 1. GitHub primitives cheat sheet

**Ephemeral runners (`--ephemeral` / `--once`)**
- Registering a runner with `--ephemeral` makes GitHub's service **automatically de-register it after it completes exactly one job** — no manual `config.sh remove` needed. [[changelog]](https://github.blog/changelog/2021-09-20-github-actions-ephemeral-self-hosted-runners-new-webhooks-for-auto-scaling/) [[trstringer]](https://trstringer.com/create-ephemeral-self-hosted-runners-github-actions/)
- **Caveat (still open, actions/runner#1337):** de-registration on GitHub's side happens, but the *local* `.runner`/`.credentials` files on disk aren't cleaned up automatically — irrelevant for a throwaway VM that gets destroyed anyway, but matters if you're reusing a base image without a fresh boot. [[issue #1337]](https://github.com/actions/runner/issues/1337)
- **VM dies mid-job or never picks up a job:** GitHub's docs don't give a hard guarantee here, and the community issue thread doesn't resolve it definitively. In practice: if a JIT-configured runner never connects, its registration is simply never "seen" as online and the job stays queued until timeout (default 24h job timeout, but the *runner startup* has its own ~10min GitHub-side wait before the job is considered unassignable and can retry another runner) — you are responsible for reaping the never-connected registration yourself (it doesn't auto-purge quickly). If it dies **mid-job**, the job shows as failed/cancelled on GitHub's side, but the runner registration can linger as "offline" until either (a) it's ephemeral and GitHub GCs it after the job outcome is recorded, or (b) you explicitly call the delete-runner API. **This is the standing reason every serious implementation pairs ephemeral+JIT with an independent reaper/TTL** rather than trusting GitHub's bookkeeping alone.

**Registration-token vs JIT config — use JIT for any new ephemeral fleet:**
- Legacy flow: `POST /repos|orgs/{owner}/{repo|org}/actions/runners/registration-token` → short-lived token → `config.sh --token ... --ephemeral`. Requires the runner VM to reach out to GitHub itself to register (two round trips: get token, then register).
- Modern flow (what every current project uses): `POST /orgs/{org}/actions/runners/generate-jitconfig` (or the repo-level equivalent) with `name`, `runner_group_id`, `labels`, `work_folder` → returns a single `encoded_jit_config` blob. The runner binary starts directly with `run.sh --jitconfig <blob>` and needs **zero further API calls** to register — better for untrusted/ephemeral VMs since no long-lived GitHub token ever needs to live on the box. [[docs]](https://docs.github.com/en/rest/actions/self-hosted-runners) [[orchestra guide]](https://www.getorchestra.io/guides/github-actions-api-create-config-for-org-just-in-time-runner)
- JIT config tokens are short-lived (~60 min) — fine for CI jobs, but there's an open issue (actions/runner#4248) about long *sequential* workflows on one JIT-registered runner outliving the token; irrelevant if each job gets its own fresh runner (the recommended pattern anyway).
- **Auth scope needed:** classic PAT needs `admin:org` (org-level) or `repo` admin rights (repo-level); fine-grained PAT / GitHub App needs the **"Self-hosted runners" organization (or repository) permission, Write** — community reports some setups additionally need Administration:Read. A GitHub App installation token is the better fit for anything long-running (auto-rotating, scoped, revocable) vs. a PAT sitting in a Lambda env var.

**Job-queue visibility:**
- **Webhook-driven (preferred by autoscalers):** subscribe to `workflow_job` with `queued`/`in_progress`/`completed` actions. This is what essentially every serious autoscaler (ARC, GARM, philips-labs, RunsOn, runner-autoscaler) uses — push-based, near-instant, doesn't touch your REST rate limit. Needs an HTTPS-reachable receiver (this is the "always-on control plane" tax).
- **Polling alternative:** `GET /repos/{o}/{r}/actions/runs?status=queued` (or `.../jobs`) — works without any receiver, but costs API calls (5000/hr authenticated PAT limit is generous for a solo repo) and has multi-second-to-minute latency depending on interval. The "workflow bootstraps its own runner" pattern sidesteps queue visibility entirely — the trigger *is* the workflow run itself.

**Runner groups/labels:** irrelevant complexity for a solo dev with one org — labels alone (`self-hosted`, `burst`, `x64`) are enough to route jobs to the right pool; runner *groups* matter for enterprises partitioning repos, not here.

## 2. Project-by-project assessment

| Project | Scale-to-zero incl. control plane? | Cleanup guarantee | Complexity | 2026 maintenance |
|---|---|---|---|---|
| **actions-runner-controller (ARC)** | **No** — needs a live Kubernetes cluster (control-plane pods + listener pods) running 24/7 regardless of job volume. [[docs]](https://docs.github.com/en/actions/concepts/runners/actions-runner-controller) | Good — k8s ephemeral pods, controller reconciles. | High — you must already run/maintain a k8s cluster. | Actively maintained by GitHub + community; v0.14.0 shipped March 2026. [[changelog]](https://github.blog/changelog/2026-03-19-actions-runner-controller-release-0-14-0/) [[repo]](https://github.com/actions/actions-runner-controller) |
| **philips-labs/terraform-aws-github-runner** | Data plane yes, **control plane no** — Lambdas + SQS + a scale-down Lambda on a cron are always provisioned, though their *cost* at idle is near-zero. Scale-down is poll-based (checks every N minutes), so cleanup is an inherent lag/brute-force sweep rather than a guarantee tied to job completion. [[repo]](https://github.com/philips-labs/terraform-aws-github-runner) [[docs]](https://philips-labs.github.io/terraform-aws-github-runner/) | Reasonable — periodic sweep terminates idle/orphan instances; historically had bugs where older `start-runner` script versions didn't call `terminate`. | Medium-high — Terraform module with many moving parts (webhook Lambda, scale-up Lambda, scale-down Lambda, sync Lambda, SSM, VPC). Real-world 2026 deployment (kane.mx writeup) still runs a persistent Lambda+SQS+S3 stack. [[kane.mx]](https://kane.mx/posts/2026/self-hosted-github-runners-aws-spot/) | Actively maintained, current in 2026 (moved to `github-aws-runners` org). |
| **garm (cloudbase/garm)** | **No** — single Go binary, but it must run *continuously* somewhere to hold webhook state and drive pools. However it's genuinely tiny (one binary, embedded SQLite, no DB/broker) and **can run on any machine you already keep on** — including this user's existing always-on home box. [[repo]](https://github.com/cloudbase/garm) | Good — pools reconcile against providers; multi-cloud pluggable providers (AWS/Azure/GCP/OpenStack/LXD/Incus/k8s). | Low-medium once running — single binary + systemd; complexity is in writing/choosing a provider plugin. | Actively maintained by Cloudbase; has an official AWS provider plugin. |
| **machulav/ec2-github-runner** (and forks) | **Yes** — the "workflow bootstraps its own runner" pattern: no infra runs when idle; it's an action invoked by a workflow on GitHub-hosted runners. | Depends entirely on your workflow's `stop-runner` step running (`if: always()`), plus a registration-token flow (older, not JIT-based) that needs a token to deregister. | Low — GitHub Action + IAM policy. | Still gets issues/updates into 2026, not archived, but has many competing forks (a sign of light/inconsistent upstream maintenance). [[repo]](https://github.com/machulav/ec2-github-runner) |
| **ubergeek77/aws-ec2-spot-runner** | **Yes** — same no-standing-infra pattern, but built specifically around **spot + JIT ephemeral + a hard failsafe TTL shutdown**. Deliberately minimal: a handful of shell scripts + `action.yml`, no Node project. | Explicit dual mechanism: (1) TTL-based hard `shutdown` timer baked into user-data as a dead-man's switch, (2) explicit deregister+terminate in the "stop" job. [[repo]](https://github.com/ubergeek77/aws-ec2-spot-runner) | **Low** — closest to the "~200-line script" ask. | Low activity/small project (few commits, 1 star) — functionally sound pattern but you're the de facto maintainer if you adopt it. |
| **RunsOn (runs-on.com)** | Very close — CloudFormation stack deployed in *your own* AWS account; the control plane is Lambda-backed and RunsOn markets it as scaling to true zero cost between runs, no per-minute fee, you only pay their license. [[repo]](https://github.com/runs-on/runs-on) | Good — core product promise (ARC alternative), spot instances, per-job runners. | **Low** for you — one CloudFormation/Terraform stack deploy, then it's "invisible." | Actively developed 2026; **free for non-commercial/open-source/personal projects** with an attribution link, or €300+/yr commercial license. [[pricing]](https://runs-on.com/pricing/) |
| **runner-autoscaler (louisgundelwein)** | **No** — requires an always-on HTTPS webhook receiver, targets **Hetzner**, not AWS. | Strong design: JIT-only (no tokens ever touch the VM), graceful drain near the billing-hour boundary, hard `MAX_RUNNER_LIFETIME_MINUTES` cap, self-healing reconciliation loop every 2 min. [[repo]](https://github.com/louisgundelwein/runner-autoscaler) | Low-medium — plain Node 22, zero runtime deps. | Actively worked on in 2026 but Hetzner-only (would need a new provider written for AWS). |

## 3. The no-standing-infra pattern ("workflow bootstraps its own runner") — deep dive

**Mechanism:** a job on a **GitHub-hosted runner** (free for public repos, cheap for private) does everything:
1. Calls `POST .../actions/runners/generate-jitconfig` (or registration-token) via the GitHub API using a repo/org secret.
2. Calls `ec2:RunInstances` with `user-data` that installs the runner agent and starts it with `--jitconfig <blob>` (or the older token flow).
3. A subsequent job (`needs: [start-runner]`, runs `on: self-hosted` with a unique label) executes your actual CI matrix on that fresh instance.
4. A final job, gated `if: always()`, calls `ec2:TerminateInstances` and (for non-JIT/non-ephemeral flows) explicitly deregisters the runner.

**Why it can fit a solo user:** the "control plane" *is* GitHub's own hosted-runner fleet — no infrastructure of yours runs at any time you're not actively burning a GitHub Actions minute, not even a Lambda. This is the only pattern with literal zero standing infrastructure including zero control-plane cost.

**Known implementations:** `machulav/ec2-github-runner` (most-forked baseline, registration-token based); `ubergeek77/aws-ec2-spot-runner` (spot + ephemeral/JIT + explicit TTL failsafe, smallest footprint); numerous GCE forks of the same idiom.

**Matrix / N-parallel runners:** the "start" job uses `strategy: matrix` (or loops `RunInstances` N times) to launch N instances each with a unique JIT config / unique label, and downstream jobs target each label. GitHub's own matrix scheduler is the fan-out mechanism.

**Cleanup-guarantee analysis (the real risk of this pattern):**
- **Happy path:** `if: always()` on the teardown job reliably runs even if the CI job fails.
- **Gap 1 — workflow cancellation:** a run-level cancel signals all not-yet-started jobs; a teardown job that hasn't started may or may not get to run depending on timing — the documented weak point across every implementation reviewed.
- **Gap 2 — runner process crash / GitHub Actions outage:** the teardown job itself needs a live GitHub Actions scheduler.
- **Gap 3 — self-hosted job never picked up** (bad AMI, network issue): the CI job hangs at "Waiting for a runner"; teardown may race its eventual timeout/cancel.
- **The universal mitigation, used by every serious implementation:** a **dead-man's switch independent of the workflow** — (a) `shutdown -h +N` scheduled at boot via user-data so the instance self-terminates after a fixed TTL regardless of what the workflow does, and/or (b) a periodic scheduled reaper that lists tagged instances, cross-checks age/registration status, and force-terminates + force-deregisters anything stale. You need at least one of these even with the no-standing-infra pattern.

## 4. Runner image (AMI) considerations

- **Prebake with Packer** vs installing at boot: prebaking (dependencies, runner binary, `runner` user with UID 1001, `/home/runner/run.sh` present) meaningfully cuts boot-to-ready time, which matters for burst capacity. [[philips-labs AMI examples]](https://philips-labs.github.io/terraform-aws-github-runner/ami-examples/) [[RunsOn Packer guide]](https://runs-on.com/guides/building-custom-ami-with-packer/)
- **Agent auto-update matters even for ephemeral runners:** GitHub stops dispatching jobs to a runner agent older than ~30 days, so a prebaked AMI needs a periodic rebuild cadence (even monthly) even though each individual runner only lives minutes.
- `--disableupdate` avoids the agent burning boot time self-updating on a machine you're about to destroy anyway — combine with the rebuild cadence above.

## 5. Comparison table

| Option | Zero standing infra (incl. control plane) | Setup effort | Ongoing maintenance | AWS-native | Cross-cloud |
|---|---|---|---|---|---|
| ARC | No (needs k8s) | High | Medium | Any k8s | Yes |
| philips-labs/terraform-aws-github-runner | Partial (Lambda/SQS always deployed, near-$0 idle) | Medium-high | Medium | Yes | No |
| garm | No (binary must run somewhere) — but free if colocated on an already-always-on box | Low-medium | Low | Yes (provider plugin) | **Yes** |
| machulav/ec2-github-runner pattern | **Yes** | Low | Low (you own the script) | Yes | Similar actions exist for GCE |
| ubergeek77/aws-ec2-spot-runner | **Yes** | **Very low** | Low | Yes | No |
| RunsOn | Near-yes (their Lambda control plane, in your account) | Low (one stack deploy) | Very low | Yes | No |
| runner-autoscaler | No (webhook receiver required) | Low-medium | Low | No (Hetzner) | No |

## 6. Top 3 recommendations for this user

**1. The no-standing-infra "bootstrap-your-own-runner" pattern, using `ubergeek77/aws-ec2-spot-runner` as a starting reference (or writing your own ~150-200 line version).** The only option with *literal* zero standing infrastructure — not even a Lambda. Use JIT config (`generate-jitconfig`) not registration tokens, spot for cost, and — critically — **add the dead-man's-switch TTL shutdown at boot** regardless of what the teardown job does, since that's the one real cleanup gap.

**2. GARM, self-hosted on the same home machine that already runs the persistent runner 24/7.** Its biggest downside (needs to run continuously) becomes a non-issue on an already-always-on box. Adds true multi-cloud burst capacity, a real reconciliation loop, single small Go binary + systemd unit — much less operational surface than ARC or the Lambda stack, while giving webhook-driven (fast) scale-up instead of polling.

**3. RunsOn, if the user wants a maintained product instead of owning a script.** Free for personal/open-source use, deploys as one CloudFormation stack in the user's own AWS account (infra ownership/IAM control retained), closest to "just works, someone else maintains it" without a Kubernetes cluster. Tradeoff: a third party's control-plane code running in your account and an attribution requirement.

**Not recommended:** ARC (needs a k8s cluster the user explicitly doesn't want), philips-labs/terraform-aws-github-runner (heavier Lambda/Terraform stack than the problem justifies), runner-autoscaler (wrong cloud, always-on receiver).

## Sources

[GitHub self-hosted runners REST API docs](https://docs.github.com/en/rest/actions/self-hosted-runners) · [Ephemeral runners changelog](https://github.blog/changelog/2021-09-20-github-actions-ephemeral-self-hosted-runners-new-webhooks-for-auto-scaling/) · [actions/runner#1337](https://github.com/actions/runner/issues/1337) · [actions/runner#4248](https://github.com/actions/runner/issues/4248) · [ARC repo](https://github.com/actions/actions-runner-controller) · [ARC 0.14.0 changelog](https://github.blog/changelog/2026-03-19-actions-runner-controller-release-0-14-0/) · [philips-labs/terraform-aws-github-runner](https://github.com/philips-labs/terraform-aws-github-runner) · [philips-labs docs](https://philips-labs.github.io/terraform-aws-github-runner/) · [kane.mx 2026 writeup](https://kane.mx/posts/2026/self-hosted-github-runners-aws-spot/) · [garm repo](https://github.com/cloudbase/garm) · [machulav/ec2-github-runner](https://github.com/machulav/ec2-github-runner) · [ubergeek77/aws-ec2-spot-runner](https://github.com/ubergeek77/aws-ec2-spot-runner) · [runs-on/runs-on repo](https://github.com/runs-on/runs-on) · [RunsOn pricing](https://runs-on.com/pricing/) · [louisgundelwein/runner-autoscaler](https://github.com/louisgundelwein/runner-autoscaler) · [RunsOn Packer AMI guide](https://runs-on.com/guides/building-custom-ami-with-packer/) · [philips-labs AMI examples](https://philips-labs.github.io/terraform-aws-github-runner/ami-examples/)
