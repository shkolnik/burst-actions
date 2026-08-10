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
| `docs/phase-4-findings.md` | First real burst vs. serial baseline: measured speedup, cost, per-phase latency. |

## Quickstart

Prebuilt binary: `burst-linux-x86_64` on the [v0.1.0 release](../../releases/tag/v0.1.0). Or
build from source with `cargo build --release`.

```
export BURST_GITHUB_TOKEN=<fine-grained PAT, Administration read/write on the target repo>
# AWS credentials via the usual env vars / profile / SSO
```

`burst.toml` in the repo you're bursting for:

```toml
[burst]
repo = "owner/repo"
```

Then: `burst bake` (once, cached after) → `burst up --auto` (or `burst up N`) → `burst status` →
fleet self-terminates → `burst sweep` reaps stragglers.

Status: **v0.1.0 released**, phases 0–4 complete. Phase 4 measured a real burst: 2.68x speedup at
essentially equal cost to serial (`docs/phase-4-findings.md`).

License: AGPL-3.0-only.
