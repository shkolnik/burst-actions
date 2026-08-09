# Phase 4 — First Real Burst & Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run a real browser-benchmark matrix through `burst up --auto` against a serial
baseline, measure wall-clock/latency/cost, produce the §8-tuning findings note and sccache
go/no-go input, ship a prebuilt binary, and replace CLAUDE.md housekeeping with real onboarding.

**Architecture:** A self-contained chromium benchmark (`bench/`) driven by one
workflow_dispatch workflow that runs either as a 6-job parallel matrix on `[self-hosted, burst]`
or as one serial job doing the same 6 units sequentially. A fixture-tested `jq` report script
turns GitHub/AWS timestamps into the phase-latency table. Two live runs (serial, then parallel
via `burst up --auto`) produce the numbers; docs and a release binary close the phase.

**Tech Stack:** Rust (existing crate, no src changes expected), bash + jq, chromium headless
under xvfb on the Debian 13 AMI, GitHub Actions workflow_dispatch, `gh` CLI.

## Global Constraints

- Every instance carries the three `burst-actions-*` tags and an armed kill schedule from
  launch (the tool enforces this; no hand-launched instances in this phase).
- Live-fleet tasks (marked **OPERATOR**) are executed inline by the session lead, never
  dispatched to subagents — they spend real money.
- Measurement runs use `instance_type = "c7i.2xlarge"` (the §8 default under test); this is a
  real-workload measurement, not a code-path test, so the smallest-type rule does not apply.
- Working config lives at the operator's gate directory (`<scratchpad>/gate/burst.toml`), repo
  `shkolnik/burst-actions`, region `us-east-2`, base_ami `ami-08c8c7f491b2217c0` (Debian 13).
- §8 defaults change only on measurement evidence, and only as findings presented to James —
  this plan never edits `src/config.rs` defaults.
- Before every commit: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings
  && cargo fmt --check`.
- Session ends with the sweep-equivalent listing (sweep + zero live `burst-actions=1`
  instances) stated in the report.
- Frugal words in all docs/output; benchmark and workflow names use the `burst-bench` stem.

---

### Task 1: Self-contained chromium benchmark workload

**Files:**
- Create: `bench/workload.html`
- Create: `bench/run-bench.sh`
- Test: local shellcheck + short-duration smoke run (chromium optional locally; full smoke
  happens on-VM in Task 4)

**Interfaces:**
- Produces: `bench/run-bench.sh TARGET_SECONDS SEED` — runs real chromium work for
  ≥ TARGET_SECONDS wall-clock, prints `BENCH_DONE seed=<SEED> chunks=<n> elapsed=<s>` on
  success, exits non-zero (loudly) on any failure. Task 2's workflow calls exactly this.

**Design:** fixed *duration*, not fixed iterations: the phase measures burst overhead (boot,
register, queue, drain) around known-length real browser work; benchmark score is not the
deliverable. Each "chunk" is one headless-chromium run over `workload.html`, which performs
deterministic layout thrash + canvas drawing + JSON/string churn sized to ~30–60 s, then
signals completion via console. The shell loop repeats chunks until TARGET_SECONDS elapsed.

- [ ] **Step 1: Write `bench/workload.html`**

```html
<!doctype html>
<meta charset="utf-8">
<title>burst-bench</title>
<canvas id="c" width="800" height="600"></canvas>
<div id="arena"></div>
<script>
// Deterministic browser workload: DOM layout thrash, canvas raster, JSON churn.
// ?iters=N scales one run; console "CHUNK_DONE" is the completion signal that
// run-bench.sh greps from chromium's --enable-logging=stderr stream.
const iters = Number(new URLSearchParams(location.search).get('iters') || 200);
const seed = Number(new URLSearchParams(location.search).get('seed') || 1);
let s = seed >>> 0;
const rnd = () => (s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32;
const arena = document.getElementById('arena');
const ctx = document.getElementById('c').getContext('2d');
let sink = 0;
function chunk(i) {
  arena.innerHTML = '';
  for (let k = 0; k < 300; k++) {
    const d = document.createElement('div');
    d.textContent = 'row ' + k + ' ' + rnd().toFixed(6);
    d.style.width = 100 + (k % 200) + 'px';
    arena.appendChild(d);
  }
  sink += arena.offsetHeight; // force layout
  for (let k = 0; k < 200; k++) {
    ctx.fillStyle = `hsl(${(k * 7) % 360},60%,50%)`;
    ctx.beginPath();
    ctx.arc(rnd() * 800, rnd() * 600, 5 + rnd() * 40, 0, 7);
    ctx.fill();
  }
  const obj = {};
  for (let k = 0; k < 500; k++) obj['k' + k] = rnd().toString(36).repeat(20);
  sink += JSON.parse(JSON.stringify(obj))['k0'].length;
  if (i + 1 < iters) setTimeout(() => chunk(i + 1), 0);
  else console.log(`CHUNK_DONE seed=${seed} sink=${sink}`);
}
chunk(0);
</script>
```

- [ ] **Step 2: Write `bench/run-bench.sh`**

```bash
#!/usr/bin/env bash
# Real-chromium benchmark driver for the burst-bench workflow. Loops
# ~30-60s chromium chunks over workload.html until TARGET_SECONDS of
# wall-clock have elapsed. Fails loud: any chromium exit without
# CHUNK_DONE aborts the job.
set -euo pipefail

target_seconds=${1:?usage: run-bench.sh TARGET_SECONDS SEED}
seed=${2:?usage: run-bench.sh TARGET_SECONDS SEED}
here=$(cd "$(dirname "$0")" && pwd)
start=$(date +%s)
chunks=0

while :; do
  elapsed=$(( $(date +%s) - start ))
  [ "$elapsed" -ge "$target_seconds" ] && break
  out=$(mktemp)
  # --enable-logging=stderr surfaces console.log; 5-min timeout bounds a
  # hung renderer well below the job/TTL layers.
  timeout 300 xvfb-run -a chromium --no-sandbox --disable-gpu \
    --enable-logging=stderr --user-data-dir="$(mktemp -d)" \
    --headless=new "file://$here/workload.html?iters=200&seed=$seed" \
    >"$out" 2>&1 || { echo "chromium chunk failed:"; tail -20 "$out"; exit 1; }
  grep -q "CHUNK_DONE seed=$seed" "$out" \
    || { echo "chunk produced no CHUNK_DONE:"; tail -20 "$out"; exit 1; }
  rm -f "$out"
  chunks=$((chunks + 1))
done

echo "BENCH_DONE seed=$seed chunks=$chunks elapsed=$(( $(date +%s) - start ))"
```

- [ ] **Step 3: Verify locally** — `chmod +x bench/run-bench.sh; shellcheck bench/run-bench.sh`
  clean. If `chromium` + `xvfb-run` exist locally, run `bench/run-bench.sh 5 1` and expect a
  `BENCH_DONE seed=1` line; if absent, note "on-VM smoke deferred to Task 4 calibration" in the
  report — do not install browsers locally.

- [ ] **Step 4: Run the four pre-commit checks** (build/test/clippy/fmt — unchanged crate must
  stay green).

- [ ] **Step 5: Commit** — `git add bench/ && git commit` message
  `bench: self-contained chromium workload for the phase-4 burst`.

---

### Task 2: burst-bench workflow (matrix + serial modes)

**Files:**
- Create: `.github/workflows/burst-bench.yml`
- Test: `gh workflow view` parse check after push (YAML syntax); live behavior is Tasks 4–5

**Interfaces:**
- Consumes: `bench/run-bench.sh TARGET_SECONDS SEED` (Task 1).
- Produces: workflow `burst-bench` with `workflow_dispatch` inputs `mode` (choice:
  `matrix`|`serial`) and `target_seconds` (default `"600"`); matrix mode = 6 parallel jobs
  named `bench (1..6)`, serial mode = 1 job `serial` running seeds 1..6 sequentially. All jobs
  `runs-on: [self-hosted, burst]`. Tasks 4–6 depend on these exact names.

- [ ] **Step 1: Write `.github/workflows/burst-bench.yml`**

```yaml
# Phase-4 measurement workflow. Two modes, same 6 work units:
#   matrix — 6 parallel jobs, one burst VM each (the product being measured)
#   serial — 1 job running all 6 units back-to-back on one VM (the baseline)
name: burst-bench
on:
  workflow_dispatch:
    inputs:
      mode:
        type: choice
        options: [matrix, serial]
        required: true
      target_seconds:
        type: string
        default: "600"
jobs:
  bench:
    if: inputs.mode == 'matrix'
    strategy:
      matrix:
        seed: [1, 2, 3, 4, 5, 6]
    runs-on: [self-hosted, burst]
    steps:
      - uses: actions/checkout@v4
      - run: bench/run-bench.sh "${{ inputs.target_seconds }}" "${{ matrix.seed }}"
  serial:
    if: inputs.mode == 'serial'
    runs-on: [self-hosted, burst]
    steps:
      - uses: actions/checkout@v4
      - run: |
          for seed in 1 2 3 4 5 6; do
            bench/run-bench.sh "${{ inputs.target_seconds }}" "$seed"
          done
```

- [ ] **Step 2: Four pre-commit checks**, then commit
  `ci: burst-bench workflow — parallel matrix vs serial baseline`.

- [ ] **Step 3: Push to origin/main** (workflow_dispatch requires the workflow on the default
  branch) and verify `gh workflow view burst-bench` succeeds.

---

### Task 3: Measurement report script

**Files:**
- Create: `bench/report.sh`
- Create: `bench/testdata/run-matrix.json` (fixture)
- Test: fixture-driven run of `report.sh` asserting exact output lines

**Interfaces:**
- Consumes: a GitHub jobs JSON (as returned by
  `gh api repos/OWNER/REPO/actions/runs/RUN_ID/jobs`) on stdin plus an hourly USD price arg.
- Produces: `bench/report.sh PRICE_PER_HOUR < jobs.json` printing, per job,
  `job=<name> queued=<s> run=<s>` and a final
  `total wall=<s> busy_instance_seconds=<s> cost_usd=<x.xxxx>` line. Tasks 5–6 read these.

**Design:** wall = max(completed_at) − min(created_at) across jobs; per-job queued =
started_at − created_at, run = completed_at − started_at; busy_instance_seconds = Σ run (VM
overhead outside job runtime is reported separately in Task 6 from burst/AWS timestamps —
this script only reduces the GitHub side). Pure `jq`, so a checked-in fixture makes it
offline-testable.

- [ ] **Step 1: Write the fixture** `bench/testdata/run-matrix.json` — two jobs with known
  timestamps:

```json
{"jobs": [
  {"name": "bench (1)", "conclusion": "success", "created_at": "2026-08-09T10:00:00Z",
   "started_at": "2026-08-09T10:02:00Z", "completed_at": "2026-08-09T10:12:00Z"},
  {"name": "bench (2)", "conclusion": "success", "created_at": "2026-08-09T10:00:30Z",
   "started_at": "2026-08-09T10:03:00Z", "completed_at": "2026-08-09T10:13:30Z"},
  {"name": "serial", "conclusion": "skipped", "created_at": "2026-08-09T10:00:00Z",
   "started_at": "2026-08-09T10:00:01Z", "completed_at": "2026-08-09T10:00:01Z"}
]}
```

(The `serial` skipped entry mirrors what the live API returns for the
mode not dispatched; the script must exclude it from every number.)

- [ ] **Step 2: Write `bench/report.sh`**

```bash
#!/usr/bin/env bash
# Reduce a GitHub run's jobs JSON (stdin) to the phase-4 latency/cost lines.
# Usage: gh api repos/O/R/actions/runs/ID/jobs | bench/report.sh 0.357
set -euo pipefail
price=${1:?usage: report.sh PRICE_PER_HOUR < jobs.json}
jq -r --arg price "$price" '
  def t: fromdateiso8601;
  .jobs
  # the mode not dispatched still appears as a skipped job — drop it
  | map(select(.conclusion != "skipped"))
  | map({name, c: (.created_at|t), s: (.started_at|t), e: (.completed_at|t)})
  | (map("job=\(.name) queued=\(.s - .c) run=\(.e - .s)") | .[]),
    "total wall=\((map(.e)|max) - (map(.c)|min)) busy_instance_seconds=\(map(.e - .s)|add) cost_usd=\((map(.e - .s)|add) * (($price|tonumber) / 3600) * 10000 | round / 10000)"
'
```

- [ ] **Step 3: Verify against the fixture** —
  `bench/report.sh 0.357 < bench/testdata/run-matrix.json` must print exactly:

```
job=bench (1) queued=120 run=600
job=bench (2) queued=150 run=630
total wall=810 busy_instance_seconds=1230 cost_usd=0.122
```

  (wall: 10:00:00→10:13:30 = 810 s; cost: 1230 s × $0.357/h = $0.12195 → 0.122.) Prove teeth
  once: temporarily change a fixture timestamp, watch the expected output disagree, revert.

- [ ] **Step 4: shellcheck + four pre-commit checks**, commit
  `bench: report.sh — jobs JSON → latency/cost table (fixture-tested)`.

---

### Task 4 (OPERATOR): calibrate one chunk + serial baseline run

Executed inline by the session lead. Config: gate `burst.toml` with
`instance_type = "c7i.2xlarge"`, `ttl_hours = 3`, `idle_timeout_min = 10`.

- [ ] **Step 1: Rebake check** — instance-type change must not rebake (key excludes it);
  expect `image cache hit` on the `up` path. If the AMI was deleted, `burst bake` first
  (~10 min).
- [ ] **Step 2: Calibration** — dispatch `burst-bench` mode=serial `target_seconds=60`, then
  `burst up 1`; confirm from job logs that chunks complete (CHUNK_DONE works on the real AMI)
  and note per-chunk seconds. This is Task 1's on-VM smoke.
- [ ] **Step 3: Serial baseline** — dispatch mode=serial `target_seconds=600`; `burst up 1`;
  let the single VM run all 6 units (~60 min). Record RUN_ID; save
  `gh api .../runs/RUN_ID/jobs` to the SDD workspace; run `bench/report.sh 0.357` on it.
- [ ] **Step 4: Verify cleanup** — VM self-terminates after its one job; `burst status` shows
  empty fleet; kill schedule gone.

---

### Task 5 (OPERATOR): the real burst — parallel matrix via `--auto`

- [ ] **Step 1: Dispatch** `burst-bench` mode=matrix `target_seconds=600` (6 queued jobs), then
  `burst up --auto` — expect `--auto` to count 6 and launch 6 (max_fleet 12 permits it).
  Record: T_launch (RunInstances), per-instance registered-at (from `up` output), GitHub run
  timestamps.
- [ ] **Step 2: Watch drain** — all 6 jobs complete, VMs self-terminate, watcher reports fleet
  gone, schedules self-cleaned. Save jobs JSON; run `bench/report.sh 0.357`.
- [ ] **Step 3: Sweep-equivalent listing** — `burst sweep` + AWS listing of `burst-actions=1`
  instances (expect none non-terminated), zero schedules, zero runner registrations.

---

### Task 6: Findings note — §8 evidence + sccache go/no-go

**Files:**
- Create: `docs/phase-4-findings.md`

**Interfaces:**
- Consumes: Task 4/5 saved jobs JSON, `report.sh` outputs, `up` output timestamps.

- [ ] **Step 1: Write the note** with exactly these sections, numbers filled from the runs (no
  placeholders survive to commit):
  - **Headline**: matrix wall-clock vs serial wall-clock (both measured), speedup factor, total
    AWS cost of the parallel run (busy-seconds cost + boot/drain overhead seconds priced at the
    same rate; name both components).
  - **Per-phase latency**: bake (hit/miss + duration), launch→registered per instance
    (min/median/max), job queued→started, last-job-done→fleet-gone (drain).
  - **§8 defaults, held or challenged**: instance type (c7i.2xlarge evidence: chunk seconds vs
    t3.micro calibration if available), volume (any disk pressure seen), idle_timeout_min /
    ttl_hours (observed margins), max_fleet. Each line: measured evidence → hold/change lean.
    Defaults change only when James rules.
  - **sccache go/no-go input** (§3 phase-2 tier): measured bake duration and boot-to-registered
    vs benchmark job length; state the lean with the arithmetic visible.
  - **Deviations**: same-hardware serial proxy (not the home runner); representative in-repo
    matrix (not James's benchmark repo) — re-run there when he provides access.
- [ ] **Step 2: Four pre-commit checks**, commit
  `docs: phase-4 findings — burst vs serial, §8 evidence, sccache input`.

---

### Task 7: Prebuilt binary release

- [ ] **Step 1:** `cargo build --release`; smoke `target/release/burst --help` (exit 2 + usage).
- [ ] **Step 2:** Tag `v0.1.0` on main, push tag, then
  `gh release create v0.1.0 target/release/burst#burst-linux-x86_64 --title "burst v0.1.0"
  --notes "First measured release: phases 0-4 complete. Linux x86_64 binary attached."`
  (gh CLI account is authorized for repo interactions).
- [ ] **Step 3:** Verify the asset downloads and runs:
  `gh release download v0.1.0 -p burst-linux-x86_64 -D /tmp-dir && chmod +x && ./burst --help`.

---

### Task 8: CLAUDE.md onboarding rewrite

**Files:**
- Modify: `CLAUDE.md` (Housekeeping section and stale phase framing)
- Modify: `README.md` (quickstart if still pre-build framed)

- [ ] **Step 1: Rewrite** — replace "nothing built"/rollout framing with current truth: phases
  0–4 complete; build/test/lint/fmt commands (keep); quickstart (burst.toml example with the
  §8-relevant keys, `bake` → `up --auto` flow, PAT + AWS env expectations); prebuilt binary
  location (release v0.1.0); pointer to `docs/phase-4-findings.md`; keep hard rails, working
  norms, reserved-for-James sections verbatim.
- [ ] **Step 2: Four pre-commit checks**, commit
  `docs: CLAUDE.md/README onboarding — tool is built; quickstart replaces rollout framing`.

---

## Execution notes

- Task order is strict: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 (3 can run before 2's push if convenient;
  4 needs 1+2 pushed, 5 needs 4's calibration, 6 needs 4+5, 8 cites 6+7).
- Budget: ~3 c7i.2xlarge instance-hours ≈ $1.10 plus one possible bake (~10 min on a
  c7i.2xlarge builder — bake uses the configured type). Abort and report if any run shows cost behavior outside this envelope.
- If `--auto` counts wrong, a VM fails registration, or chromium fails on the real AMI: stop,
  root-cause first, one hypothesis at a time (working norms); three failed fixes → James.
