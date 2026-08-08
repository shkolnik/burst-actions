# burst-runners

On-demand ephemeral cloud VMs as GitHub Actions self-hosted runners: one command launches a fleet
from a prebaked image, the fleet drains the job queue, then unregisters, terminates, and provably
cleans up. Scale-to-zero, no standing infrastructure, no AWS account pre-setup, no
Terraform/Ansible.

Not specific to any one repo — it applies to beep-browser CI (where the need surfaced:
heavy-but-parallelizable benchmark and xvfb-static pipelines forced serial on one home runner) but
is a general tool.

- `design-proposal.md` — **start here.** The consolidated design + recommendation (status:
  proposal, awaiting James's read; open calls in its §3/§5).
- `research/` — the three commissioned takes the proposal synthesizes:
  - `homelab-landscape.md` — what homelab/solo users use for cloud-burst CI (Sonnet, web research)
  - `gha-ecosystem.md` — GitHub primitives + autoscaler project assessments (Sonnet, web research)
  - `first-principles-design.md` — from-scratch design, no research required (Fable)

This directory is a temporary home; it may become a standalone project repo or move elsewhere.
Working context/memory for this effort lives with the beep-browser project.
