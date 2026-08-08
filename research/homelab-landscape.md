# Ephemeral Cloud-Burst GitHub Actions Runners — Homelab/Solo-Dev Landscape (Aug 2026)

*Commissioned research report (Sonnet, web research). One of three inputs to `../design-proposal.md`.*

## Landscape

**philips-labs/terraform-aws-github-runner → github-aws-runners/terraform-aws-github-runner** — The long-standing reference implementation: Terraform module deploying Lambdas + an SQS-driven scale-up/scale-down pipeline on AWS, spot or on-demand EC2, ephemeral runners, scale-to-zero by default. Complexity: **moderate-to-high** — it's a real serverless system (multiple Lambdas, SSM parameters, webhook receiver, optional org-level pooling) meant for teams running lots of concurrent jobs. Maintenance: the original repo was **archived Jan 2025 and moved to a new community org `github-aws-runners`**, which is a real signal — treat it as "alive but mid-migration," verify the new org's release cadence before betting on it. ([github.com](https://github.com/philips-labs/terraform-aws-github-runner))

**RunsOn (runs-on.com)** — A single CloudFormation stack (or Terraform/OpenTofu module) that stands up a lightweight control plane in your own AWS account; workflows opt in by changing `runs-on:` labels. Launches one ephemeral EC2 runner per job, terminates it after, scale-to-zero is the default steady state, ships an S3-backed cache compatible with `actions/cache`. Complexity: **low** — this is the closest thing to "the tool this user is describing," explicitly positioned as the simple alternative to ARC/Kubernetes. Not free: flat annual license from €300/yr for commercial use (free for non-commercial/OSS use), everything else (EC2/EBS/S3) billed at raw AWS rates with no per-minute markup. Actively developed — 1.3k stars, 750+ commits, continuous blog/doc updates through 2026. ([github.com](https://github.com/runs-on/runs-on))

**machulav/ec2-github-runner** — A pair of GitHub Actions (`start` / `stop`) you drop into your own workflow YAML: `start` launches an EC2 instance from an AMI and registers it (token or JIT), your job runs, `stop` (run with `if: always()`) deregisters and terminates it. Supports spot via `market-type: spot`, and JIT/ephemeral registration so a runner self-deregisters after one job. Complexity: **very low** — no infra to deploy at all, it's just Actions steps + an IAM role. This is the "small script against the AWS CLI" end of the spectrum, basically pre-packaged. Popular (850+ stars, ~50 forks) but the README itself flags the weak point: cleanup depends on the `stop` job actually running — there's no independent dead-man's-switch baked in, so a hard crash/cancelled workflow can orphan an instance. ([github.com](https://github.com/machulav/ec2-github-runner))

**GARM (Cloudbase, cloudbase/garm)** — A single self-hosted binary (embedded SQLite, no external DB) that manages runner *pools* across AWS, Azure, GCP, OpenStack, OCI, LXD/Incus, Kubernetes, etc. via pluggable "external providers," and as of 2026 supports GitHub's native Actions Runner Scale Sets (long-polling, no webhook receiver needed). Complexity: **moderate** — more infrastructure than RunsOn/ec2-github-runner (you run and babysit the GARM daemon itself), but simpler than ARC and genuinely multi-cloud. Actively maintained in 2026 (Go module updates, ARC-scale-set support landed this year). Best fit if cross-cloud flexibility actually matters to this user later. ([github.com](https://github.com/cloudbase/garm))

**actions-runner-controller (ARC, GitHub's own k8s controller)** — Ephemeral runner *pods*, one per job, scale-to-zero, officially GitHub-maintained. Complexity: **high** — requires an existing (or newly stood-up) Kubernetes cluster, and securing that cluster is itself nontrivial. Consensus from write-ups: overkill for a single developer/homelab unless you already run k8s or need private-network access from CI. ([docs.github.com](https://docs.github.com/en/actions/concepts/runners/actions-runner-controller))

**Cirun.io** — Hosted SaaS control plane that spins up ephemeral VMs in *your* AWS/GCP/Azure/Oracle account per job, config via a `.cirun.yml` label mapping. Free for public/OSS repos with unlimited runners; private-repo tiers scale by repo count ($29–$499/mo), not usage. Complexity: **low** (you outsource the control plane) but it's a third-party dependency sitting in the middle of your CI. Actively developed as of 2026. ([betterstack.com](https://betterstack.com/community/comparisons/cirun-alternatives/))

**gha-runner (ethanholz), TestFlows-GitHub-Hetzner-Runners, terraform-aws-github-runners (cloudandthings), various forks** — Smaller community projects, generally thinner wrappers around the same pattern (spin up cloud VM → register → run → terminate). Worth knowing they exist as prior art, not worth adopting sight-unseen — maintenance varies widely and wasn't independently confirmed here.

**Hosted "just pay per minute" alternatives** (WarpBuild, Namespace, Tenki, Blacksmith) — Not self-hosted at all; you rent their fleet. Notable 2026 market shakeout: **BuildJet and Cirrus CI both shut down in H1 2026**, which is a useful data point on vendor durability risk for a "must work for years unattended" requirement — self-hosting on your own cloud account avoids that failure mode entirely. ([tenki.cloud](https://tenki.cloud/blog/github-actions-runner-showdown-2026))

## What homelab users actually converge on, and why

The pattern that keeps showing up in blog posts and the DIY-script genre is deliberately small: **cloud-init user-data that (1) fetches a JIT/registration token, (2) configures the runner with `--ephemeral`, (3) starts it, and relies on the runner process itself exiting after one job** — combined with **`--instance-initiated-shutdown-behavior=terminate`** so that when the runner (or a fallback `shutdown` timer) powers the OS off, AWS tears down the instance automatically rather than leaving it billing indefinitely. This is exactly the shape of `machulav/ec2-github-runner`'s internals and of most from-scratch blog implementations (e.g. RunsOn's own "simple cloud-init script" writeup, the `terma` gist for spot-instance runners). Homelab/solo-dev users converge on this rather than Terraform-module systems because: (a) it's legible — one shell script you can read top to bottom; (b) it has no standing infrastructure to patch/upgrade between bursts; (c) ephemeral + spot means a crashed run just burns pennies, not a persistent leak. Where people *do* reach for a packaged tool, RunsOn and Cirun get cited most for "I don't want to write this myself but don't want Kubernetes either" — and ARC/philips-labs-style Terraform modules get recommended specifically for orgs with high, sustained parallel job volume, which this user's "occasional burst" profile doesn't match. ([runs-on.com](https://runs-on.com/blog/1-how-to-setup-github-hosted-runner-with-a-simple-cloud-init-script/), [gist.github.com](https://gist.github.com/terma/32e0787217bc21deaa7c70fa3b3c06c9))

## Cost notes

- GitHub-hosted standard Linux minute: ~$0.006/min. Larger hosted runners (Jan 2026 rate cut, up to 39% off): 4-core $0.012/min, 8-core $0.022/min, 16-core $0.042/min, 32-core $0.082/min, 64-core $0.162/min.
- Self-hosted spot EC2 equivalent: a `c8g.2xlarge` spot instance (~$0.13/hr) works out to roughly $0.0022/min — about **2.7x cheaper than a standard hosted minute**, and the gap widens sharply on larger runner sizes since AWS's per-vCPU spot pricing scales far more slowly than GitHub's per-minute tiers. Typical spot discount vs on-demand is 60–75%.
- GitHub floated a $0.002/min "control-plane fee" for self-hosted runners in Dec 2025, but **postponed it after pushback**; as of Aug 2026 there is no such fee in effect and no confirmed return date — worth rechecking periodically since it changes the calculus if reinstated.
- Net: for a bursty, parallelizable workload (browser automation benchmarks, Xvfb builds), spot EC2 self-hosting stays the clear cost winner even before counting that it also removes any GitHub Actions minutes quota pressure. ([cicdcost.com](https://cicdcost.com/github-actions-pricing-changes-2026), [kane.mx](https://kane.mx/posts/2026/self-hosted-github-runners-aws-spot/))

## DIY pattern (canonical minimal shape)

A from-scratch script/workflow for this use case typically has these pieces, none of them exotic:

1. **Trigger**: either (a) a `workflow_dispatch`/scheduled "burst" job kicks off the whole start→run→stop sequence as three jobs in one workflow (the `ec2-github-runner` pattern), or (b) an external poller (cron on the home Proxmox box, or a tiny Lambda on a schedule) checks the GitHub Actions queue depth via the REST API and launches capacity only when jobs are actually queued.
2. **Registration token**: fetched at launch time via the GitHub API (`POST /repos/{owner}/{repo}/actions/runners/registration-token`, short-lived) or — preferred in 2026 — **JIT config** (`generate-jitconfig`), which mints a single-use, pre-scoped runner config so no long-lived PAT needs to live on the instance.
3. **Ephemeral flag**: runner started with `--ephemeral`, meaning it accepts exactly one job then exits on its own — this is what makes "idle until queue empty" trivial: the process lifecycle *is* the idle detector, no polling needed on the runner side.
4. **Idle/self-termination**: cloud-init sets `instance-initiated-shutdown-behavior=terminate` at launch, and either (a) the runner's own exit triggers `shutdown -h now`, or (b) a belt-and-suspenders **dead-man's-switch timer** (`shutdown -h +N` scheduled at boot, cancelled only if the runner is still actively working) guarantees termination even if the runner process hangs or GitHub never dispatches a job.
5. **Spot instance**: `market-type=spot` (or an ASG with mixed instances policy for the pool-based tools), chosen because ephemeral single-job runners are short-lived enough that interruption risk is low and the discount is large.
6. **Cleanup guarantee / dead-man's switch**: the pattern people layer on top of "the happy path terminates itself" is a **separate scheduled Lambda/cron sweep** that lists EC2 instances tagged e.g. `github-runner=true` older than N minutes and force-terminates + deregisters any GitHub-side runner entry left dangling — this is the actual answer to "leaked resources," since relying solely on in-instance shutdown logic is exactly what orphans instances when a script step fails mid-flight.
7. **Deregistration**: a `stop` step (or the sweep job) calls `DELETE /repos/{owner}/{repo}/actions/runners/{runner_id}` so GitHub's runner list doesn't accumulate stale offline entries (GitHub does eventually GC these itself after ~30 days idle, but a proactive delete keeps the UI honest).

## Pitfalls discovered in the wild

- **Cleanup depends on a step that can itself fail to run** — `machulav/ec2-github-runner`'s own docs stress `if: always()` on the stop job precisely because a cancelled workflow, runner crash, or GitHub outage skips it; the tool has no independent watchdog, so people who adopt it as-is still eventually find an orphaned instance. Treat the shutdown-behavior=terminate + dead-man's-switch layer as **mandatory**, not optional hardening.
- **Orphaned runner *entries* in GitHub's UI** persist even after the EC2 instance is gone, until GitHub's own ~30-day cooling-off GC or a manual `gh api` delete sweep. Not costly, but noisy/confusing over time.
- **Security surface of self-hosted runners on public repos**: documented real-world hijacks of AWS-hosted GitHub runners via PR-triggered workflows — relevant only if the user ever runs `pull_request_target`-style workflows on this fleet against untrusted forks; scope the IAM role tightly regardless.
- **Spot capacity/interruption fallback**: several tools (ec2-github-runner, the AWS blog patterns) recommend multi-AZ or spot→on-demand fallback so a burst benchmark run doesn't stall waiting on capacity.
- **philips-labs repo migration**: a maintained-looking, heavily-linked project (terraform-aws-github-runner) got archived and moved orgs in the last ~18 months — a reminder to check "is this still the canonical repo" before depending on any single community tool for years.
- **Vendor risk on hosted alternatives**: BuildJet and Cirrus CI both discontinued in H1 2026 — self-hosting on your own AWS account is the only option in this space with no third-party existential risk.

## Top 3 recommendations for this user

1. **Start with a from-scratch script following the `ec2-github-runner` pattern (or literally use that Action), hardened with a scheduled sweep Lambda/cron for orphan cleanup.** Matches the stated preference exactly — small AWS-CLI-level script, ephemeral + spot + JIT tokens, no new infra to maintain — and the sweep closes the one real gap (orphaned instances/runner entries) that bites everyone using this pattern.
2. **If the DIY route feels like more surface area than wanted, adopt RunsOn instead of building it.** It's the one packaged option that matches "mature but simple" — single CloudFormation stack, scale-to-zero by construction, no Kubernetes, stays inside the user's own AWS account (so no vendor-shutdown risk like BuildJet/Cirrus), and the license cost (€300/yr, free for non-commercial/OSS) is cheap relative to engineering time for a solo OSS project.
3. **Skip ARC/Kubernetes and skip philips-labs' Terraform module entirely** — both are built for sustained multi-team job volume, not an occasional heavy-but-parallel burst from one project; the operational overhead (cluster security, Lambda/SSM pipeline maintenance) isn't justified here, and philips-labs' recent org migration adds near-term uncertainty this user doesn't need to absorb.

## Sources

- [terraform-aws-github-runner README](https://github.com/philips-labs/terraform-aws-github-runner/blob/develop/README.md)
- [philips-labs/terraform-aws-github-runner (MOVED)](https://github.com/philips-labs/terraform-aws-github-runner)
- [machulav/ec2-github-runner](https://github.com/machulav/ec2-github-runner)
- [Saving time and money with self-hosted runners on EC2](https://getunblocked.com/blog/ec2-self-hosted-runners/)
- [runs-on/runs-on](https://github.com/runs-on/runs-on)
- [RunsOn features](https://runs-on.com/features/)
- [RunsOn: simple cloud-init script](https://runs-on.com/blog/1-how-to-setup-github-hosted-runner-with-a-simple-cloud-init-script/)
- [cloudbase/garm](https://github.com/cloudbase/garm)
- [Manage your own GitHub runners using garm — Cloudbase](https://cloudbase.it/manage-your-own-github-runners-using-garm/)
- [Actions Runner Controller — GitHub Docs](https://docs.github.com/en/actions/concepts/runners/actions-runner-controller)
- [Cirun.io Alternatives — Better Stack](https://betterstack.com/community/comparisons/cirun-alternatives/)
- [AWS EC2 Spot Instances for GitHub Self-hosted runners (gist)](https://gist.github.com/terma/32e0787217bc21deaa7c70fa3b3c06c9)
- [drewmmiranda/aws-kill-switch](https://github.com/drewmmiranda/aws-kill-switch)
- [How to delete orphaned GitHub Actions runners with GitHub CLI](https://hn.mrugesh.dev/how-to-delete-orphaned-github-actions-runners-with-github-cli)
- [Hijacking AWS-Hosted GitHub Runners](https://onsecurity.io/article/hijacking-aws-hosting-github-runners/)
- [GitHub Actions Runner Showdown 2026 — Tenki](https://tenki.cloud/blog/github-actions-runner-showdown-2026)
- [CI Runner Cost in 2026 — CICDCost.com](https://cicdcost.com/runner-cost)
- [GitHub Actions Pricing Changes 2026 — CICDCost.com](https://cicdcost.com/github-actions-pricing-changes-2026)
- [Self-Hosted GitHub Runners on AWS Spot for AI Dev Teams](https://kane.mx/posts/2026/self-hosted-github-runners-aws-spot/)
