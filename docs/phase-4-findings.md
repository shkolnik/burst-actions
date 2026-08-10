# Phase 4 findings: first real burst

Measured 2026-08-10, c7i.2xlarge (8 vCPU), us-east-2, on-demand $0.357/hr, Debian 13 AMI cache
hit (`ami-0c62038c29210c104`, key `v1-25d66bb72f498bcb`). All numbers below are measured from the
operator runs (`task-6-data.md`) unless marked inferred.

## Headline

- Matrix wall-clock: **1365s**. Serial wall-clock: **3656s**. Speedup: **2.68x**.
- Cost: parallel $0.36 (busy-seconds $0.3581 equivalent + ~$0.006/instance boot/drain overhead at
  the same rate) vs serial $0.3581 busy-seconds cost — **same cost, 2.68x faster**.
- The run hit a vCPU quota cap (32 vCPU account limit vs 48 requested), capping the fleet at 4 of
  6 instances and forcing a second wave. Inferred uncapped case (6 instances, one wave): wall
  ≈ 660s (~54-59s launch-to-pickup + ~605s job) → **≈5.5x** speedup. Not measured — quota was hit
  every real run.

## Per-phase latency

- **Bake**: cache hit, no rebake. An instance-type change alone did not invalidate the cache.
  Miss-path duration not exercised this run — not measured.
- **Launch→registered per instance**: ~54-59s (min 54s, median ~56-57s, max 59s), covering
  launch, boot, JIT registration, and job pickup on a warm AMI cache. Confirmed by a separate
  calibration run (31342894074) showing the same shape.
- **Job queued→started**: wave 1 (within quota) 45-59s per job. Wave 2 (waiting on quota
  headroom freed by wave-1 drain) 758-760s — this reflects the quota wait, not per-instance
  latency.
- **Last-job-done→fleet-gone (drain)**: not captured as a discrete duration this run. Observed
  qualitatively: every VM self-terminated after exactly one job, 0 tagged instances remained
  after each run, and the wave-1 watcher exited 0 after a clean drain.

## §8 defaults: held or challenged

- **instance_type (c7i.2xlarge)**: hold. Job ran to completion in ~605s as expected; no evidence
  requiring a change (data has no t3.micro calibration comparison for this workload).
- **volume**: hold. No disk pressure reported.
- **idle_timeout_min=10**: hold. Never fired — job pickup was ~1min, well inside the window.
- **ttl_hours=3**: hold. Never fired — jobs ran ~10min.
- **max_fleet**: hold on the setting itself, but see quota preflight below — the account's vCPU
  quota (32), not `max_fleet`, was the binding constraint this run.
- **quota preflight**: challenged. `--auto` assumed quota headroom until launch time; the cap was
  discovered only when it launched 4 of 6 requested instances. The warning that fired was loud
  and named a remedy (quota-increase console pointer), and the tool recovered correctly (second
  `burst up --auto` picked up the remaining 2) — so this is not a correctness bug. Whether to add
  a pre-launch quota check is reserved for James: option A (query quota before launching, fail
  fast with the same remedy message) vs option B (keep launch-time detection, since it already
  degrades gracefully with a fleet split rather than a failure). Lean: A, since a pre-launch
  check would avoid wave splitting and its 700s+ tax on affected jobs, but this is not James's
  call to make silently.

## sccache go/no-go input

Measured fixed per-instance overhead: ~55s + $0.006. The phase-4 benchmark is a browser workload,
not a compile, so it doesn't exercise sccache's use case directly. Arithmetic: for a job whose
cold `cargo build` dominates, ~55s of fixed overhead is small relative to a multi-minute build,
and the $0.006/instance cost is negligible against burst's own economics. sccache (S3 backend)
would add standing infrastructure (a bucket) — reserved for James per the decision log. Lean:
**not needed for burst's own economics** based on this data; a future Rust-heavy benchmark would
be the right trigger to revisit.

## Cleanup evidence (from real runs, not unit tests)

Every VM self-terminated after exactly one job (0 tagged instances after each run). Watchers were
harness-killed twice post-launch; orphan kill schedules (1 serial + 2 wave-2) were disarmed by
`burst sweep`. 0 leaked runner registrations. One gap seen live, recorded for James and not
redesigned around: reconcile-on-entry dropped a dead statefile record (`i-0f1450b94ee47e9a4`)
before sweep's registration tidy could match minted runner names; the registration check found no
actual leak this time, but the ordering is worth James's attention.

## Deviations from the target scenario

- Serial baseline used the same c7i.2xlarge hardware, not James's actual home runner — this is a
  same-hardware proxy, not a comparison against the real baseline machine.
- The matrix job was a representative in-repo benchmark, not James's browser-benchmark repo.
  Re-run against the real repo when he provides access.
