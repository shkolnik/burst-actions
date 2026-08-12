# burst-actions

On-demand ephemeral cloud VMs as GitHub Actions self-hosted runners: one command (`burst up`)
launches a fleet from a prebaked AMI, the fleet drains the job queue one job per VM, then
unregisters, terminates, and provably cleans up. Scale-to-zero, no standing infrastructure, no
AWS account pre-setup, no Terraform/Ansible, genuinely open source.

Built for the solo maintainer whose one fast home runner handles everyday CI but forces
heavy-parallelizable pipelines (benchmark matrices, big build fans) to run serial. Not specific
to any one repo — one invocation serves one repo; many repos use the tool.

| File | What it is |
|---|---|
| `CLAUDE.md` | Onboarding for the working agent — read first in a fresh session. |
| `implementation-phases.md` | The four implementation phases and their verification gates. |
| `design-proposal.md` | **The approved design.** Requirements, landscape verdict, full design, decision log, rollout plan, starting defaults. |
| `research/` | Preserved research: the three original commissioned takes (`homelab-landscape.md`, `gha-ecosystem.md`, `first-principles-design.md`), the adversarial buy-vs-build review (`adversarial-runson-review.md`), and the OSS-only landscape sweep (`oss-landscape-sweep.md`). The design proposal supersedes all of them where they disagree. |
| `docs/runner-contract.md` | **For consuming repos.** What a job can rely on: disk, timeouts, credentials, the `provision` key. |
| `docs/phase-4-findings.md` | First real burst vs. serial baseline: measured speedup, cost, per-phase latency. |

## Quickstart

Prebuilt binary: `burst-linux-x86_64` on the [latest release](../../releases/latest), built
and provenance-attested by CI (`.github/workflows/release.yml`; verify with
`gh attestation verify burst-linux-x86_64 --repo shkolnik/burst-actions`). Or build from source
with `cargo build --release`.

```
export BURST_GITHUB_TOKEN=<fine-grained PAT, Administration read/write on the target repo>
# AWS credentials via the usual env vars / profile / SSO
```

In the repo you're bursting for:

```
burst init owner/repo   # writes an annotated burst.toml; edit what you need
```

Then: `burst bake` (once, cached after) → `burst up --auto` (or `burst up N`) → `burst status` →
fleet self-terminates → `burst sweep` reaps stragglers.

What a job gets — one VM per job, a single sized gp3 root volume, the two timeout layers, and the
`provision` hook for extra packages — is `docs/runner-contract.md`.

## Configuration

`repo` is the only required setting; everything else has a default. This is the file `burst init`
writes, quoted verbatim from `config.example.toml` (a test fails if the two drift, and another
fails if a setting exists in the code but not here).

<details>
<summary><code>config.example.toml</code></summary>

```toml
# burst configuration.
#
# `repo` is the only required setting. Every other line is commented out and
# shows the default burst uses when you leave it that way; uncomment (drop the
# leading `#`) to change one.
#
# A job reaches these runners by asking for them in the workflow:
#     runs-on: [self-hosted, burst]
# What that job can then rely on — disk, timeouts, credentials — is
# https://github.com/shkolnik/burst-actions/blob/main/docs/runner-contract.md

[burst]
# The repository whose queued jobs this fleet serves. Required; `--repo` on the
# command line overrides it.
repo = "owner/repo"

# ---- fleet ------------------------------------------------------------------

# EC2 instance type for every runner, and for the AMI bake builder.
#instance_type = "c7i.2xlarge"

# Largest fleet `burst up --auto` will launch at once. A hard cap on how much
# this tool can spend per invocation; the account's vCPU quota may bind first.
#max_fleet = 12

# AWS region. Defaults to whatever your AWS profile or environment resolves to.
# The AMI is region-bound, so changing regions means a full rebake.
#region = "us-east-2"

# Runner CPU architecture: "x86_64" or "arm64".
#arch = "x86_64"

# ---- disk -------------------------------------------------------------------
# The VM has one gp3 root volume; the job workspace and the container data root
# both live on it. The runner-contract link above documents the guarantee and
# how to assert it from a job.

# Root volume size in GiB (gp3: 1-16384). Size for your job's measured peak
# plus headroom.
#volume_gb = 100

# Provisioned IOPS (gp3: 3000-16000, at most 500 per GiB). Unset takes gp3's
# 3000 baseline.
#volume_iops = 6000

# Provisioned throughput in MB/s (gp3: 125-1000, at most 0.25 per provisioned
# IOPS). Unset takes gp3's 125 baseline, which is under an hour per 500 GiB
# written — raise it for write-heavy jobs.
#volume_throughput_mbps = 1000

# ---- timeouts ---------------------------------------------------------------

# Minutes a runner may wait for a job before powering itself off. Applies only
# before a job is claimed — a long-running quiet job is never "idle".
#idle_timeout_min = 10

# Hard cap on VM uptime, unconditional on what the VM is doing. Keep it above
# your longest job's `timeout-minutes` plus boot.
#ttl_hours = 6

# ---- image ------------------------------------------------------------------

# Base AMI to bake from. Never guessed: `burst bake` fails with the exact line
# to paste here, so run it once and copy that line in.
#base_ami = "ami-0703709356caf3a36"

# Shell script appended to burst's provisioning script and run as root at bake
# time, for packages your jobs need. Path is relative to this file. Its bytes
# are part of the image cache key, so editing it forces a rebake.
#provision = ".burst/provision.sh"

# ---- cost -------------------------------------------------------------------

# Monthly AWS Budgets alarm in USD. Opt-in: absent means no alarm is created.
#budget_alarm_usd = 15
```

</details>

Status: **v0.1.0 released**, phases 0–4 complete. Phase 4 measured a real burst: 2.68x speedup at
essentially equal cost to serial (`docs/phase-4-findings.md`).

License: AGPL-3.0-only.
