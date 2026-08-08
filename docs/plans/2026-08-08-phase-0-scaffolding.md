# Phase 0: Project Scaffolding — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** A building, linting, format-checked, LSP-ready `burst` crate skeleton with dependabot
configured — so phase 1 starts on rails and no tooling gap is discovered mid-feature.

**Architecture:** lib+bin crate; all dependencies phase 1 needs declared now (they're already
decided in the phase-1 plan — declaring them here avoids Cargo.toml churn per task). Lint
policy lives in `[lints]` in Cargo.toml so `cargo clippy` and CI agree by construction.

**Tech Stack:** Rust ≥ 1.89, edition 2024, rustup-managed stable toolchain with clippy, rustfmt,
rust-analyzer components.

## Global Constraints

- Crate/binary named `burst`, `license = "AGPL-3.0-only"`, standalone (never a workspace member).
- Commit messages end with the two trailer lines the team lead uses (copy from `git log -1`).
- Every check runs against the real toolchain — no "should work"; paste actual output in the
  task report.

---

### Task 1: Cargo scaffold + toolchain pin + lint/format config

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/lib.rs`, `.gitignore`, `rust-toolchain.toml`,
  `rustfmt.toml`

**Interfaces:**
- Produces: a crate that `cargo build && cargo test && cargo clippy -- -D warnings && cargo fmt --check`
  passes clean; phase 1 Task 1 adds the real CLI on top.

- [ ] **Step 1: Write the files**

`Cargo.toml`:

```toml
[package]
name = "burst"
version = "0.1.0"
edition = "2024"
rust-version = "1.89"
license = "AGPL-3.0-only"
description = "On-demand ephemeral cloud VMs as GitHub Actions self-hosted runners"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "2"
sha2 = "0.10"
hex = "0.4"
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"

[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
# Closed sets are matched exhaustively — a `_ =>` arm on our own enums is the
# stale-branch bug the compiler exists to catch (CLAUDE.md engineering philosophy).
wildcard_enum_match_arm = "warn"
dbg_macro = "warn"
todo = "warn"
unimplemented = "warn"
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt", "rust-analyzer"]
```

`rustfmt.toml`:

```toml
max_width = 100
```

`src/main.rs`:

```rust
fn main() {}
```

`src/lib.rs`:

```rust
// burst — on-demand ephemeral GitHub Actions runners. AGPL-3.0-only.
```

`.gitignore`:

```
/target
```

- [ ] **Step 2: Verify** — run and paste output of:

```bash
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
rustc --version   # must be ≥ 1.89
```

If the environment's rustc is < 1.89 or rustup is absent, STOP and report — do not lower
`rust-version` or vendor a toolchain.

- [ ] **Step 3: Commit** — `chore: cargo scaffold — edition 2024, lint policy in [lints], toolchain pin, rustfmt`.

---

### Task 2: rust-analyzer for the Claude Code LSP

**Files:** none in-repo (toolchain component + verification only).

- [ ] **Step 1:** `rust-analyzer --version` — if missing, `rustup component add rust-analyzer`,
  then re-run and paste the version. (The rust-toolchain.toml from Task 1 lists the component,
  so a rustup-managed environment installs it on first `cargo` invocation; this step verifies
  rather than assumes.)
- [ ] **Step 2:** Confirm the LSP resolves against the built crate: from the repo root, open
  `src/lib.rs` via the harness LSP tooling (or `rust-analyzer --help` + a `cargo check` pass as
  the fallback proof) and report what was actually exercised. No commit unless a config file
  proved necessary — if one did, commit it with a message saying why.

---

### Task 3: Dependabot + CI workflow

**Files:**
- Create: `.github/dependabot.yml`, `.github/workflows/ci.yml`

These are inert until the GitHub remote exists (James is setting it up) — committing them now
means the repo is born with CI and dependency updates on first push.

- [ ] **Step 1: Write the files**

`.github/dependabot.yml`:

```yaml
version: 2
updates:
  - package-ecosystem: cargo
    directory: /
    schedule:
      interval: weekly
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: weekly
```

`.github/workflows/ci.yml`:

```yaml
name: ci
on:
  push:
  pull_request:
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
```

- [ ] **Step 2: Verify** — `python3 -c "import yaml,sys; [yaml.safe_load(open(f)) for f in ['.github/dependabot.yml','.github/workflows/ci.yml']]"`
  (or any YAML parse check available); paste output. The workflow itself can only truly run
  after push — say so in the report, verified-vs-inferred.

- [ ] **Step 3: Commit** — `chore: dependabot (cargo + actions) and CI workflow — live at first push`.

---

## Phase gate

`cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
all pass on the committed tree; `rust-analyzer --version` works; dependabot + CI YAML parse.
