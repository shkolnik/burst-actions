# Phase 1: Local Core & Contracts — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `burst` crate's cloud-free foundation: CLI surface, config, statefile+lock,
schema types, the `Cloud` trait with an in-memory fake, and adoption reconciliation — all
verified offline.

**Architecture:** lib+bin crate (`src/lib.rs` exposes modules; `src/main.rs` is thin CLI
dispatch) so library-level tests exercise everything without the binary and nothing is
dead-code. All cloud interaction goes through the five-method `Cloud` trait; phase 1 ships only
the in-memory `FakeCloud` test backend. No AWS/GitHub call, no tokio yet — the trait is sync in
phase 1 and gets async-ified in phase 2 when `aws-sdk-rust` arrives (internal seam, cheap to
reverse; we don't pay for tokio before it's used).

**Tech Stack:** Rust ≥ 1.89 (std file locking), edition 2024. clap 4 (derive), serde +
serde_json, toml 0.8, thiserror 2, sha2 + hex, chrono (serde feature). Dev: assert_cmd,
predicates, tempfile.

## Global Constraints

- Binary and crate named `burst`; `license = "AGPL-3.0-only"`; **never a workspace member of
  any consumer project** (standalone repo — it already is; don't add workspace stanzas).
- Tags exactly: `burst=1`, `burst-repo=<owner/repo>`, `burst-expires=<ISO8601>` (RFC 3339 UTC).
- §8 defaults verbatim: idle timeout 10 min, hard TTL 6 h, instance type `c7i.2xlarge`,
  max_fleet 12, arch x86_64.
- State dir: `$XDG_STATE_HOME/burst/<owner>-<repo>/` falling back to
  `~/.local/state/burst/<owner>-<repo>/`.
- Closed sets are exhaustively-matched enums, never strings compared with `==`. No `_ =>`
  arm on our own enums.
- Fail loud, never degrade: every error states what's wrong and what to do. One authoring site
  per message (e.g. one `unimplemented_cmd()` helper, not five inline strings).
- Statefile writes are write-then-rename, always.
- Unimplemented subcommands exit 1 with an explicit "not implemented yet" — never silently
  no-op. Exit codes: 2 = usage (clap default), 1 = everything else; we promise no finer codes
  yet.
- Every commit message honest; guard tests proven by regressing production code once (Task 9).
- Commit messages end with the two trailer lines the team lead uses (Co-Authored-By +
  Claude-Session); copy them from `git log -1`.

---

### Task 1: CLI surface

**Files:**
- Modify: `src/main.rs` (phase 0 left it a stub); Create: `tests/cli.rs`

**Interfaces:**
- Consumes: the phase-0 scaffold (`docs/plans/2026-08-08-phase-0-scaffolding.md`) — crate
  builds clean with all dependencies already declared.
- Produces: binary `burst` with subcommands `up [N] [--auto] [--spot] [--yes] [--ssh-key K]`,
  `bake`, `status`, `down [--yes]`, `sweep`; global `--repo <owner/repo>`. Every subcommand
  currently exits 1 via `not_implemented(cmd: &str) -> !` printing
  `burst <cmd>: not implemented yet (see implementation-phases.md)`.

- [ ] **Step 1: Write failing CLI tests** in `tests/cli.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

fn burst() -> Command {
    Command::cargo_bin("burst").unwrap()
}

#[test]
fn no_args_prints_usage() {
    burst().assert().code(2).stderr(predicate::str::contains("Usage"));
}

#[test]
fn up_requires_n_or_auto() {
    burst()
        .args(["up", "--repo", "octo/widgets"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--auto"));
}

#[test]
fn up_n_and_auto_conflict() {
    burst()
        .args(["up", "3", "--auto", "--repo", "octo/widgets"])
        .assert()
        .code(2);
}

#[test]
fn subcommands_fail_loud_not_silent() {
    for cmd in ["bake", "status", "sweep"] {
        burst()
            .args([cmd, "--repo", "octo/widgets"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("not implemented yet"));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --test cli`. Expected: assertion
  failures (phase 0's `main.rs` stub compiles but exits 0 with no output).

- [ ] **Step 3: Implement** `src/main.rs` (replacing the phase-0 stub):

```rust
use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "burst", version, about = "Ephemeral cloud VMs as GitHub Actions runners")]
struct Cli {
    /// Target GitHub repository as owner/repo (overrides burst.toml)
    #[arg(long, global = true)]
    repo: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Launch runner VMs: a count, or --auto to size from the queued jobs
    Up {
        /// Number of VMs to launch
        #[arg(conflicts_with = "auto", required_unless_present = "auto")]
        n: Option<u32>,
        /// Size the fleet from the queued burst-labeled jobs
        #[arg(long)]
        auto: bool,
        /// Use spot instances
        #[arg(long)]
        spot: bool,
        /// Skip interactive confirmations (for automation)
        #[arg(long)]
        yes: bool,
        /// EC2 key pair name to allow SSH debug access
        #[arg(long)]
        ssh_key: Option<String>,
    },
    /// Build or rebuild the runner AMI
    Bake,
    /// Show live fleet state (cloud truth)
    Status,
    /// Terminate this repo's fleet
    Down {
        #[arg(long)]
        yes: bool,
    },
    /// Reap expired instances, orphan schedules, dead registrations
    Sweep,
}

fn not_implemented(cmd: &str) -> ExitCode {
    eprintln!("burst {cmd}: not implemented yet (see implementation-phases.md)");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Up { .. } => not_implemented("up"),
        Cmd::Bake => not_implemented("bake"),
        Cmd::Status => not_implemented("status"),
        Cmd::Down { .. } => not_implemented("down"),
        Cmd::Sweep => not_implemented("sweep"),
    }
}
```

- [ ] **Step 4: Run** `cargo test --test cli` — all 4 pass. Also
  `cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat: full CLI surface, all subcommands fail loud"`
  (plus the trailer lines from Global Constraints).

---

### Task 2: Schema — RepoId, tags, Arch, image key

**Files:**
- Create: `src/schema.rs`; Modify: `src/lib.rs` (add `pub mod schema;`)

**Interfaces:**
- Produces:
  - `schema::RepoId` — `RepoId::parse(&str) -> Result<RepoId, Error>`;
    `fn owner(&self) -> &str`; `fn name(&self) -> &str`; `impl Display` prints `owner/repo`;
    `fn slug(&self) -> String` prints `owner-repo`.
  - `schema::TAG_BURST = "burst"`, `TAG_REPO = "burst-repo"`, `TAG_EXPIRES = "burst-expires"`.
  - `schema::TagSpec { repo: RepoId, expires: DateTime<Utc> }` with
    `fn to_tags(&self) -> [(String, String); 3]` (values `"1"`, `owner/repo`, RFC 3339 UTC).
  - `schema::Arch` enum `{ X86_64, Arm64 }`, `fn as_str(&self) -> &'static str`
    (`"x86_64"` / `"arm64"`), `impl Default` = `X86_64`.
  - `schema::ImageKeyInputs<'a> { provisioning_script: &'a [u8], base_image_id: &'a str, arch: Arch, runner_agent_version: &'a str }`
    and `schema::image_key(&ImageKeyInputs) -> String` (`"v1-"` + 16 hex chars).
  - `error::Error` (create `src/error.rs`, `pub mod error;`): thiserror enum with variants
    used so far: `RepoInvalid { given: String }` (message names the expected `owner/repo`
    form), `NotImplemented { cmd: &'static str }`.

- [ ] **Step 1: Failing unit tests** (bottom of `src/schema.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn repo_id_parses_and_slugs() {
        let r = RepoId::parse("octo/widgets").unwrap();
        assert_eq!(r.owner(), "octo");
        assert_eq!(r.name(), "widgets");
        assert_eq!(r.to_string(), "octo/widgets");
        assert_eq!(r.slug(), "octo-widgets");
    }

    #[test]
    fn repo_id_rejects_malformed() {
        for bad in ["", "noslash", "a//b", "/x", "x/", "a/b/c", "we ird/repo"] {
            assert!(RepoId::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn tag_spec_emits_exact_schema() {
        let expires = chrono::Utc.with_ymd_and_hms(2026, 8, 8, 18, 0, 0).unwrap();
        let t = TagSpec { repo: RepoId::parse("octo/widgets").unwrap(), expires };
        assert_eq!(
            t.to_tags(),
            [
                ("burst".into(), "1".into()),
                ("burst-repo".into(), "octo/widgets".into()),
                ("burst-expires".into(), "2026-08-08T18:00:00+00:00".into()),
            ]
        );
    }

    #[test]
    fn image_key_stable_and_input_sensitive() {
        let base = ImageKeyInputs {
            provisioning_script: b"#!/bin/sh\napt-get install -y foo\n",
            base_image_id: "ami-0abc",
            arch: Arch::X86_64,
            runner_agent_version: "2.320.0",
        };
        let k = image_key(&base);
        assert_eq!(k, image_key(&base), "key must be deterministic");
        assert!(k.starts_with("v1-") && k.len() == 3 + 16, "{k}");
        for changed in [
            ImageKeyInputs { provisioning_script: b"#!/bin/sh\n", ..base },
            ImageKeyInputs { base_image_id: "ami-0abd", ..base },
            ImageKeyInputs { arch: Arch::Arm64, ..base },
            ImageKeyInputs { runner_agent_version: "2.321.0", ..base },
        ] {
            assert_ne!(k, image_key(&changed));
        }
    }

    #[test]
    fn image_key_fields_are_delimited() {
        // "ab" + "c" must not hash equal to "a" + "bc"
        let a = ImageKeyInputs {
            provisioning_script: b"ab",
            base_image_id: "c",
            arch: Arch::X86_64,
            runner_agent_version: "v",
        };
        let b = ImageKeyInputs { provisioning_script: b"a", base_image_id: "bc", ..a };
        assert_ne!(image_key(&a), image_key(&b));
    }
}
```

- [ ] **Step 2: Run** `cargo test schema` — fails to compile (types missing).

- [ ] **Step 3: Implement** `src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid repository {given:?}: expected owner/repo (letters, digits, . _ -)")]
    RepoInvalid { given: String },
    #[error("burst {cmd}: not implemented yet (see implementation-phases.md)")]
    NotImplemented { cmd: &'static str },
}
```

and `src/schema.rs`:

```rust
use crate::error::Error;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::fmt;

pub const TAG_BURST: &str = "burst";
pub const TAG_REPO: &str = "burst-repo";
pub const TAG_EXPIRES: &str = "burst-expires";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    owner: String,
    name: String,
}

impl RepoId {
    pub fn parse(s: &str) -> Result<Self, Error> {
        let err = || Error::RepoInvalid { given: s.to_string() };
        let (owner, name) = s.split_once('/').ok_or_else(err)?;
        let ok = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        if !ok(owner) || !ok(name) {
            return Err(err());
        }
        Ok(RepoId { owner: owner.to_string(), name: name.to_string() })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn slug(&self) -> String {
        format!("{}-{}", self.owner, self.name)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone)]
pub struct TagSpec {
    pub repo: RepoId,
    pub expires: DateTime<Utc>,
}

impl TagSpec {
    pub fn to_tags(&self) -> [(String, String); 3] {
        [
            (TAG_BURST.into(), "1".into()),
            (TAG_REPO.into(), self.repo.to_string()),
            (
                TAG_EXPIRES.into(),
                self.expires.to_rfc3339_opts(SecondsFormat::Secs, false),
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arch {
    #[default]
    X86_64,
    Arm64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageKeyInputs<'a> {
    pub provisioning_script: &'a [u8],
    pub base_image_id: &'a str,
    pub arch: Arch,
    pub runner_agent_version: &'a str,
}

/// Content-addressed image cache key: v1- + 8 bytes of SHA-256 over the
/// length-prefixed inputs (length prefix so field boundaries can't alias).
pub fn image_key(i: &ImageKeyInputs) -> String {
    let mut h = Sha256::new();
    for field in [
        i.provisioning_script,
        i.base_image_id.as_bytes(),
        i.arch.as_str().as_bytes(),
        i.runner_agent_version.as_bytes(),
    ] {
        h.update((field.len() as u64).to_be_bytes());
        h.update(field);
    }
    format!("v1-{}", hex::encode(&h.finalize()[..8]))
}
```

`src/lib.rs` becomes:

```rust
// burst — on-demand ephemeral GitHub Actions runners. AGPL-3.0-only.
pub mod error;
pub mod schema;
```

- [ ] **Step 4: Run** `cargo test schema` — all pass; `cargo clippy -- -D warnings`.

- [ ] **Step 5: Commit** — `feat: schema types — RepoId, tag triple, Arch, content-addressed image key`.

---

### Task 3: Config — burst.toml + defaults + validation

**Files:**
- Create: `src/config.rs`; Modify: `src/lib.rs` (`pub mod config;`), `src/error.rs`

**Interfaces:**
- Consumes: `schema::{RepoId, Arch}`, `error::Error`.
- Produces:
  - `config::Config { repo: RepoId, instance_type: String, region: Option<String>, max_fleet: u32, idle_timeout_min: u32, ttl_hours: u32, arch: Arch, base_ami: Option<String>, provision: Option<PathBuf> }`
  - `config::load(dir: &Path, repo_flag: Option<&str>) -> Result<Config, Error>` — reads
    `<dir>/burst.toml` if present (`[burst]` table, unknown keys are a hard error), applies
    defaults, `repo_flag` overrides the file's `repo`; missing repo from both → error naming
    both remedies.
  - New `error::Error` variants:
    `ConfigRead { path: PathBuf, source: std::io::Error }`,
    `ConfigInvalid { path: PathBuf, reason: String }`,
    `RepoMissing` (message: `no repository: pass --repo owner/repo or set repo in burst.toml`).

- [ ] **Step 1: Failing tests** (in `src/config.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Arch;

    fn dir_with(toml: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("burst.toml"), toml).unwrap();
        d
    }

    #[test]
    fn defaults_apply_with_flag_repo_and_no_file() {
        let d = tempfile::tempdir().unwrap();
        let c = load(d.path(), Some("octo/widgets")).unwrap();
        assert_eq!(c.repo.to_string(), "octo/widgets");
        assert_eq!(c.instance_type, "c7i.2xlarge");
        assert_eq!(c.max_fleet, 12);
        assert_eq!(c.idle_timeout_min, 10);
        assert_eq!(c.ttl_hours, 6);
        assert_eq!(c.arch, Arch::X86_64);
        assert!(c.region.is_none() && c.base_ami.is_none() && c.provision.is_none());
    }

    #[test]
    fn file_values_load_and_flag_overrides_repo() {
        let d = dir_with(
            "[burst]\nrepo = \"a/b\"\ninstance_type = \"c7i.4xlarge\"\nmax_fleet = 3\n",
        );
        let c = load(d.path(), None).unwrap();
        assert_eq!(c.repo.to_string(), "a/b");
        assert_eq!(c.instance_type, "c7i.4xlarge");
        assert_eq!(c.max_fleet, 3);
        let c2 = load(d.path(), Some("octo/widgets")).unwrap();
        assert_eq!(c2.repo.to_string(), "octo/widgets");
    }

    #[test]
    fn missing_repo_names_both_remedies() {
        let d = tempfile::tempdir().unwrap();
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("--repo") && e.contains("burst.toml"), "{e}");
    }

    #[test]
    fn unknown_key_is_a_hard_error() {
        let d = dir_with("[burst]\nrepo = \"a/b\"\ninstance_typo = \"x\"\n");
        let e = load(d.path(), None).unwrap_err().to_string();
        assert!(e.contains("instance_typo"), "{e}");
    }

    #[test]
    fn zero_limits_rejected() {
        for bad in ["max_fleet = 0", "idle_timeout_min = 0", "ttl_hours = 0"] {
            let d = dir_with(&format!("[burst]\nrepo = \"a/b\"\n{bad}\n"));
            assert!(load(d.path(), None).is_err(), "accepted {bad}");
        }
    }
}
```

- [ ] **Step 2: Run** `cargo test config` — compile failure.

- [ ] **Step 3: Implement** `src/config.rs`:

```rust
use crate::error::Error;
use crate::schema::{Arch, RepoId};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    burst: BurstTable,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BurstTable {
    repo: Option<String>,
    instance_type: Option<String>,
    region: Option<String>,
    max_fleet: Option<u32>,
    idle_timeout_min: Option<u32>,
    ttl_hours: Option<u32>,
    arch: Option<String>,
    base_ami: Option<String>,
    provision: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub repo: RepoId,
    pub instance_type: String,
    pub region: Option<String>,
    pub max_fleet: u32,
    pub idle_timeout_min: u32,
    pub ttl_hours: u32,
    pub arch: Arch,
    pub base_ami: Option<String>,
    pub provision: Option<PathBuf>,
}

pub fn load(dir: &Path, repo_flag: Option<&str>) -> Result<Config, Error> {
    let path = dir.join("burst.toml");
    let table = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|source| Error::ConfigRead { path: path.clone(), source })?;
        toml::from_str::<FileConfig>(&text)
            .map_err(|e| Error::ConfigInvalid { path: path.clone(), reason: e.to_string() })?
            .burst
    } else {
        BurstTable::default()
    };

    let invalid = |reason: String| Error::ConfigInvalid { path: path.clone(), reason };

    let repo = match repo_flag.or(table.repo.as_deref()) {
        Some(r) => RepoId::parse(r)?,
        None => return Err(Error::RepoMissing),
    };
    let arch = match table.arch.as_deref() {
        None => Arch::default(),
        Some("x86_64") => Arch::X86_64,
        Some("arm64") => Arch::Arm64,
        Some(other) => return Err(invalid(format!("arch {other:?}: expected x86_64 or arm64"))),
    };
    let nonzero = |name: &str, v: Option<u32>, default: u32| match v {
        Some(0) => Err(invalid(format!("{name} must be at least 1"))),
        Some(n) => Ok(n),
        None => Ok(default),
    };

    Ok(Config {
        repo,
        instance_type: table.instance_type.unwrap_or_else(|| "c7i.2xlarge".into()),
        region: table.region,
        max_fleet: nonzero("max_fleet", table.max_fleet, 12)?,
        idle_timeout_min: nonzero("idle_timeout_min", table.idle_timeout_min, 10)?,
        ttl_hours: nonzero("ttl_hours", table.ttl_hours, 6)?,
        arch,
        base_ami: table.base_ami,
        provision: table.provision,
    })
}
```

New `error.rs` variants:

```rust
    #[error("cannot read {path}: {source}", path = .path.display())]
    ConfigRead { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("invalid config {path}: {reason}", path = .path.display())]
    ConfigInvalid { path: std::path::PathBuf, reason: String },
    #[error("no repository: pass --repo owner/repo or set repo in burst.toml")]
    RepoMissing,
```

- [ ] **Step 4: Run** `cargo test config` — pass; clippy clean.

- [ ] **Step 5: Wire the binary** so config errors surface today: in `main.rs`, before
  dispatch, call `burst::config::load(&std::env::current_dir()?, cli.repo.as_deref())` — on
  error print `error: {e}` to stderr and exit 1; on success pass `Config` into the (still
  `not_implemented`) command arms. Add to `tests/cli.rs`:

```rust
#[test]
fn missing_repo_fails_with_remedy() {
    let d = tempfile::tempdir().unwrap();
    burst()
        .current_dir(d.path())
        .args(["status"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--repo"));
}

#[test]
fn unknown_config_key_fails_loud() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("burst.toml"), "[burst]\nrepo=\"a/b\"\nbogus=1\n").unwrap();
    burst()
        .current_dir(d.path())
        .args(["status"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("bogus"));
}
```

  (add `tempfile` use to the test file; note existing `subcommands_fail_loud_not_silent` /
  `up_*` tests must now pass `--repo` — they already do — and run in a temp cwd so a stray
  `burst.toml` can't interfere: give `burst()` a
  `.current_dir` on a shared `tempfile::TempDir`.)

- [ ] **Step 6: Run** `cargo test` — all pass.

- [ ] **Step 7: Commit** — `feat: burst.toml config with §8 defaults, unknown-key and zero-limit hard errors, wired into CLI`.

---

### Task 4: Statefile — atomic JSON manifest

**Files:**
- Create: `src/state.rs`; Modify: `src/lib.rs` (`pub mod state;`), `src/error.rs`

**Interfaces:**
- Consumes: `schema::RepoId`, `error::Error`.
- Produces:
  - `state::InstanceRecord { pub id: String, pub launched_at: DateTime<Utc>, pub expires_at: DateTime<Utc> }`
    (serde Serialize/Deserialize, `Debug, Clone, PartialEq`).
  - `state::StateFile { pub version: u32, pub repo: String, pub instances: Vec<InstanceRecord> }`
    — `version` is currently always `1`; reading any other version is `StateCorrupt`.
  - `state::RepoState` — `RepoState::open(repo: &RepoId) -> Result<RepoState, Error>` (resolves
    root from `$XDG_STATE_HOME` else `$HOME/.local/state`, joins `burst/<slug>`, creates the
    dir) and `RepoState::open_at(dir: PathBuf) -> RepoState` (tests / explicit root; no env);
    methods `read(&self) -> Result<Option<StateFile>, Error>`,
    `write(&self, &StateFile) -> Result<(), Error>` (serialize pretty → `state.json.tmp` →
    fsync → rename to `state.json`), `delete(&self) -> Result<(), Error>` (ok if absent).
  - New `Error` variants: `State { path: PathBuf, source: std::io::Error }`,
    `StateCorrupt { path: PathBuf, reason: String }`,
    `Environment { reason: String }` (e.g. neither XDG_STATE_HOME nor HOME set).

- [ ] **Step 1: Failing tests** (in `src/state.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample() -> StateFile {
        StateFile {
            version: 1,
            repo: "octo/widgets".into(),
            instances: vec![InstanceRecord {
                id: "i-0123".into(),
                launched_at: Utc::now(),
                expires_at: Utc::now(),
            }],
        }
    }

    #[test]
    fn write_read_roundtrip_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        assert!(rs.read().unwrap().is_none());
        rs.write(&sample()).unwrap();
        assert_eq!(rs.read().unwrap().unwrap().instances[0].id, "i-0123");
        rs.delete().unwrap();
        assert!(rs.read().unwrap().is_none());
        rs.delete().unwrap(); // idempotent
    }

    #[test]
    fn write_is_rename_based_so_a_crashed_write_leaves_old_state() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        rs.write(&sample()).unwrap();
        // Model a crash between tmp-write and rename: a half-written tmp file exists.
        std::fs::write(d.path().join("state.json.tmp"), b"{\"version\":1,\"repo").unwrap();
        let read = rs.read().unwrap().unwrap();
        assert_eq!(read.instances.len(), 1, "reader must see the old committed state");
        // And no tmp residue is ever read as state.
    }

    #[test]
    fn corrupt_statefile_is_a_loud_error_not_empty() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        std::fs::write(d.path().join("state.json"), b"not json").unwrap();
        assert!(matches!(rs.read(), Err(Error::StateCorrupt { .. })));
    }

    #[test]
    fn unknown_version_is_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        let mut s = sample();
        s.version = 2;
        rs.write(&s).unwrap();
        assert!(matches!(rs.read(), Err(Error::StateCorrupt { .. })));
    }

    #[test]
    fn open_respects_xdg_state_home() {
        let d = tempfile::tempdir().unwrap();
        // env mutation: safe here because cargo runs tests in-process threads —
        // serialize by using a unique var read at call time via helper.
        let root = state_root_from(Some(d.path().as_os_str().into()), None).unwrap();
        assert_eq!(root, d.path().join("burst"));
        let home = state_root_from(None, Some("/home/u".into())).unwrap();
        assert_eq!(home, std::path::Path::new("/home/u/.local/state/burst"));
        assert!(state_root_from(None, None).is_err());
    }
}
```

- [ ] **Step 2: Run** `cargo test state` — compile failure.

- [ ] **Step 3: Implement** `src/state.rs`:

```rust
use crate::error::Error;
use crate::schema::RepoId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub id: String,
    pub launched_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub version: u32,
    pub repo: String,
    pub instances: Vec<InstanceRecord>,
}

pub struct RepoState {
    dir: PathBuf,
}

pub(crate) fn state_root_from(
    xdg: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, Error> {
    if let Some(x) = xdg {
        return Ok(PathBuf::from(x).join("burst"));
    }
    if let Some(h) = home {
        return Ok(PathBuf::from(h).join(".local/state/burst"));
    }
    Err(Error::Environment {
        reason: "neither XDG_STATE_HOME nor HOME is set; cannot locate the burst state dir"
            .into(),
    })
}

impl RepoState {
    pub fn open(repo: &RepoId) -> Result<Self, Error> {
        let root = state_root_from(
            std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()),
            std::env::var_os("HOME").filter(|v| !v.is_empty()),
        )?;
        let dir = root.join(repo.slug());
        std::fs::create_dir_all(&dir)
            .map_err(|source| Error::State { path: dir.clone(), source })?;
        Ok(RepoState { dir })
    }

    pub fn open_at(dir: PathBuf) -> Self {
        RepoState { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn read(&self) -> Result<Option<StateFile>, Error> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::State { path, source }),
        };
        let state: StateFile = serde_json::from_str(&text)
            .map_err(|e| Error::StateCorrupt { path: path.clone(), reason: e.to_string() })?;
        if state.version != STATE_VERSION {
            return Err(Error::StateCorrupt {
                path,
                reason: format!("unknown statefile version {}", state.version),
            });
        }
        Ok(Some(state))
    }

    pub fn write(&self, state: &StateFile) -> Result<(), Error> {
        let tmp = self.dir.join("state.json.tmp");
        let err = |source| Error::State { path: tmp.clone(), source };
        let mut f = std::fs::File::create(&tmp).map_err(err)?;
        f.write_all(serde_json::to_string_pretty(state).expect("statefile serializes").as_bytes())
            .map_err(err)?;
        f.sync_all().map_err(err)?;
        std::fs::rename(&tmp, self.path())
            .map_err(|source| Error::State { path: self.path(), source })?;
        Ok(())
    }

    pub fn delete(&self) -> Result<(), Error> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::State { path: self.path(), source }),
        }
    }
}
```

New `Error` variants:

```rust
    #[error("state file {path}: {source}", path = .path.display())]
    State { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("state file {path} is corrupt ({reason}) — if no burst fleet is live, delete it; if one is, run burst status", path = .path.display())]
    StateCorrupt { path: std::path::PathBuf, reason: String },
    #[error("{reason}")]
    Environment { reason: String },
```

- [ ] **Step 4: Run** `cargo test state` — pass; clippy clean.

- [ ] **Step 5: Commit** — `feat: statefile — versioned JSON manifest, write-then-rename, loud corruption errors`.

---

### Task 5: Repo lock — flock, fail-fast, evaporates with the process

**Files:**
- Modify: `src/state.rs`, `src/error.rs`

**Interfaces:**
- Consumes: Task 4's `RepoState`.
- Produces:
  - `state::RepoLock` — RAII guard; lock released on drop (and by the OS on process death).
  - `RepoState::lock(&self) -> Result<RepoLock, Error>` — opens/creates `<dir>/lock`,
    `File::try_lock()`; `WouldBlock` → `Error::LockHeld { repo_dir: PathBuf }` whose message
    says another burst invocation is running for this repo (and that a crashed one would have
    released it).
  - New `Error` variant `LockHeld { repo_dir: PathBuf }`.

- [ ] **Step 1: Failing tests** (append to `src/state.rs` tests):

```rust
    #[test]
    fn second_lock_fails_fast_while_first_held() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        let _held = rs.lock().unwrap();
        // flock is per open-file-description, so a second open in this process conflicts.
        let rs2 = RepoState::open_at(d.path().to_path_buf());
        assert!(matches!(rs2.lock(), Err(Error::LockHeld { .. })));
    }

    #[test]
    fn lock_releases_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        drop(rs.lock().unwrap());
        assert!(rs.lock().is_ok(), "dropped lock must be re-acquirable");
    }

    #[test]
    fn abandoned_run_signal_is_statefile_present_plus_lock_acquirable() {
        let d = tempfile::tempdir().unwrap();
        let rs = RepoState::open_at(d.path().to_path_buf());
        rs.write(&sample()).unwrap();
        // No live holder: the lock acquires AND state is present → residue to adopt.
        let _lock = rs.lock().unwrap();
        assert!(rs.read().unwrap().is_some());
    }
```

- [ ] **Step 2: Run** `cargo test state` — compile failure (`lock` missing).

- [ ] **Step 3: Implement** (append to `src/state.rs`):

```rust
pub struct RepoLock {
    _file: std::fs::File,
}

impl RepoState {
    pub fn lock(&self) -> Result<RepoLock, Error> {
        let path = self.dir.join("lock");
        let file = std::fs::File::create(&path)
            .map_err(|source| Error::State { path: path.clone(), source })?;
        match file.try_lock() {
            Ok(()) => Ok(RepoLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(Error::LockHeld { repo_dir: self.dir.clone() })
            }
            Err(std::fs::TryLockError::Error(source)) => Err(Error::State { path, source }),
        }
    }
}
```

New `Error` variant:

```rust
    #[error("another burst invocation is already running for this repo (lock held in {repo_dir}); a crashed run would have released it — wait for or stop the other invocation", repo_dir = .repo_dir.display())]
    LockHeld { repo_dir: std::path::PathBuf },
```

Note: the OS drops the flock when the holding process dies — that is invariant 7's
"statefile present + lock acquirable = abandoned" signal; the third test pins it as far as a
single process can. True cross-process fail-fast through the compiled binary becomes testable
in phase 3 when `up` holds the lock across a watch; `implementation-phases.md` records this
deferral.

- [ ] **Step 4: Run** `cargo test state` — pass; clippy clean.

- [ ] **Step 5: Commit** — `feat: per-repo flock with fail-fast LockHeld and RAII release`.

---

### Task 6: The Cloud trait + FakeCloud

**Files:**
- Create: `src/cloud/mod.rs`, `src/cloud/fake.rs`; Modify: `src/lib.rs` (`pub mod cloud;`)

**Interfaces:**
- Consumes: `schema::{RepoId, TagSpec, TAG_BURST, TAG_REPO, TAG_EXPIRES}`, `error::Error`.
- Produces (`src/cloud/mod.rs`):

```rust
use crate::error::Error;
use crate::schema::{RepoId, TagSpec};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    Pending,
    Running,
    ShuttingDown,
    Terminated,
}

impl InstanceState {
    pub fn is_live(self) -> bool {
        match self {
            InstanceState::Pending | InstanceState::Running => true,
            InstanceState::ShuttingDown | InstanceState::Terminated => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub state: InstanceState,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub count: u32,
    pub image_id: String,
    pub instance_type: String,
    pub spot: bool,
    pub tags: TagSpec,
    pub user_data: String,
}

/// The only seam to a cloud. Sync in phase 1; phase 2 makes these async when
/// aws-sdk-rust arrives.
pub trait Cloud {
    /// Launch spec.count instances with spec.tags applied atomically at creation.
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error>;
    fn terminate(&mut self, ids: &[String]) -> Result<(), Error>;
    /// All non-terminated instances carrying burst=1 and burst-repo=<repo>.
    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error>;
    /// Arm the control-plane one-shot kill for one instance at `at`.
    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error>;
    /// Get-or-create the image for `key`; returns the image id.
    fn bake(&mut self, key: &str) -> Result<String, Error>;
}

pub mod fake;
```

- `fake::FakeCloud` (`Default`): implements `Cloud`; also test helpers
  `fn set_state(&mut self, id: &str, s: InstanceState)`,
  `fn armed_kills(&self) -> &[(String, DateTime<Utc>)]`,
  `fn plant(&mut self, instance: Instance)` (pre-seed an instance the fake didn't launch —
  simulates another host's fleet). Launch mints ids `i-fake-0`, `i-fake-1`, … in
  `InstanceState::Running` with `spec.tags.to_tags()` applied; `bake` returns
  `format!("ami-fake-{key}")` and is idempotent per key.

- [ ] **Step 1: Failing tests** (in `src/cloud/fake.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RepoId, TagSpec, TAG_REPO};
    use chrono::{Duration, Utc};

    fn spec(repo: &str, count: u32) -> LaunchSpec {
        LaunchSpec {
            count,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec {
                repo: RepoId::parse(repo).unwrap(),
                expires: Utc::now() + Duration::hours(6),
            },
            user_data: "jit-blob".into(),
        }
    }

    #[test]
    fn launch_applies_tags_atomically_and_lists_by_repo() {
        let mut c = FakeCloud::default();
        let launched = c.launch(&spec("octo/widgets", 2)).unwrap();
        assert_eq!(launched.len(), 2);
        for i in &launched {
            assert!(i.tags.iter().any(|(k, v)| k == TAG_REPO && v == "octo/widgets"));
        }
        c.launch(&spec("other/repo", 1)).unwrap();
        let listed = c.list_tagged(&RepoId::parse("octo/widgets").unwrap()).unwrap();
        assert_eq!(listed.len(), 2, "must filter by burst-repo");
    }

    #[test]
    fn terminated_instances_leave_the_listing() {
        let mut c = FakeCloud::default();
        let ids: Vec<String> =
            c.launch(&spec("octo/widgets", 2)).unwrap().into_iter().map(|i| i.id).collect();
        c.terminate(&ids[..1]).unwrap();
        let listed = c.list_tagged(&RepoId::parse("octo/widgets").unwrap()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ids[1]);
    }

    #[test]
    fn arm_kill_records_per_instance_schedules() {
        let mut c = FakeCloud::default();
        let at = Utc::now() + Duration::hours(6);
        let launched = c.launch(&spec("octo/widgets", 1)).unwrap();
        c.arm_kill(&launched[0].id, at).unwrap();
        assert_eq!(c.armed_kills(), &[(launched[0].id.clone(), at)]);
    }

    #[test]
    fn bake_is_get_or_create_per_key() {
        let mut c = FakeCloud::default();
        let a = c.bake("v1-abc").unwrap();
        assert_eq!(a, c.bake("v1-abc").unwrap());
        assert_ne!(a, c.bake("v1-def").unwrap());
    }
}
```

- [ ] **Step 2: Run** `cargo test cloud` — compile failure.

- [ ] **Step 3: Implement** `src/cloud/fake.rs`:

```rust
use super::{Cloud, Instance, InstanceState, LaunchSpec};
use crate::error::Error;
use crate::schema::{RepoId, TAG_REPO};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct FakeCloud {
    instances: Vec<Instance>,
    kills: Vec<(String, DateTime<Utc>)>,
    images: BTreeMap<String, String>,
    next_id: u32,
}

impl FakeCloud {
    pub fn set_state(&mut self, id: &str, s: InstanceState) {
        let i = self
            .instances
            .iter_mut()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no such fake instance {id}"));
        i.state = s;
    }

    pub fn armed_kills(&self) -> &[(String, DateTime<Utc>)] {
        &self.kills
    }

    pub fn plant(&mut self, instance: Instance) {
        self.instances.push(instance);
    }
}

impl Cloud for FakeCloud {
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error> {
        let mut out = Vec::new();
        for _ in 0..spec.count {
            let instance = Instance {
                id: format!("i-fake-{}", self.next_id),
                state: InstanceState::Running,
                tags: spec.tags.to_tags().into_iter().collect(),
            };
            self.next_id += 1;
            self.instances.push(instance.clone());
            out.push(instance);
        }
        Ok(out)
    }

    fn terminate(&mut self, ids: &[String]) -> Result<(), Error> {
        for i in &mut self.instances {
            if ids.contains(&i.id) {
                i.state = InstanceState::Terminated;
            }
        }
        Ok(())
    }

    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error> {
        let repo = repo.to_string();
        Ok(self
            .instances
            .iter()
            .filter(|i| i.state != InstanceState::Terminated)
            .filter(|i| i.tags.iter().any(|(k, v)| k == TAG_REPO && *v == repo))
            .cloned()
            .collect())
    }

    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error> {
        self.kills.push((instance_id.to_string(), at));
        Ok(())
    }

    fn bake(&mut self, key: &str) -> Result<String, Error> {
        Ok(self.images.entry(key.to_string()).or_insert_with(|| format!("ami-fake-{key}")).clone())
    }
}
```

(`src/cloud/mod.rs` is exactly the Interfaces block above.)

- [ ] **Step 4: Run** `cargo test cloud` — pass; clippy clean.

- [ ] **Step 5: Commit** — `feat: five-method Cloud trait + in-memory FakeCloud test backend`.

---

### Task 7: Adoption reconciliation

**Files:**
- Create: `src/reconcile.rs`; Modify: `src/lib.rs` (`pub mod reconcile;`)

**Interfaces:**
- Consumes: `state::{StateFile, InstanceRecord}`, `cloud::{Instance, InstanceState}`,
  `schema::TAG_EXPIRES`.
- Produces:

```rust
pub struct Reconciled {
    /// The full post-reconciliation manifest: statefile records still alive in
    /// the cloud, plus adopted ones.
    pub live: Vec<InstanceRecord>,
    /// Ids present in the cloud but absent from the statefile (tags are
    /// authoritative — adopted into `live`).
    pub adopted: Vec<String>,
    /// Ids present in the statefile but no longer alive — dropped.
    pub dropped: Vec<String>,
}

pub fn reconcile(state: &StateFile, cloud: &[Instance]) -> Reconciled
```

Rules (design §3 "Concurrency and resumption"): a statefile record whose instance is not in
`cloud` with a live state → `dropped`; a live cloud instance not in the statefile → `adopted`,
with `expires_at` parsed from its `burst-expires` tag (unparsable/missing tag → treat as
already expired: `expires_at = launched_at = DateTime::UNIX_EPOCH`, so the sweep reaps it
rather than trusting an untagged stranger — but it still lists in `live` because it exists).

- [ ] **Step 1: Failing tests** (in `src/reconcile.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{Instance, InstanceState};
    use crate::state::{InstanceRecord, StateFile};
    use chrono::{TimeZone, Utc};

    fn rec(id: &str) -> InstanceRecord {
        let t = Utc.with_ymd_and_hms(2026, 8, 8, 12, 0, 0).unwrap();
        InstanceRecord { id: id.into(), launched_at: t, expires_at: t }
    }

    fn inst(id: &str, state: InstanceState, expires: Option<&str>) -> Instance {
        let mut tags = vec![
            ("burst".to_string(), "1".to_string()),
            ("burst-repo".to_string(), "octo/widgets".to_string()),
        ];
        if let Some(e) = expires {
            tags.push(("burst-expires".to_string(), e.to_string()));
        }
        Instance { id: id.into(), state, tags }
    }

    fn state_of(ids: &[&str]) -> StateFile {
        StateFile {
            version: 1,
            repo: "octo/widgets".into(),
            instances: ids.iter().map(|i| rec(i)).collect(),
        }
    }

    #[test]
    fn gone_instances_drop_unknown_live_ones_adopt() {
        let state = state_of(&["i-a", "i-b"]);
        let cloud = vec![
            inst("i-a", InstanceState::Running, Some("2026-08-08T18:00:00+00:00")),
            inst("i-c", InstanceState::Running, Some("2026-08-08T18:00:00+00:00")),
        ];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.dropped, vec!["i-b"]);
        assert_eq!(r.adopted, vec!["i-c"]);
        let mut live: Vec<&str> = r.live.iter().map(|i| i.id.as_str()).collect();
        live.sort();
        assert_eq!(live, vec!["i-a", "i-c"]);
        let c = r.live.iter().find(|i| i.id == "i-c").unwrap();
        assert_eq!(c.expires_at, Utc.with_ymd_and_hms(2026, 8, 8, 18, 0, 0).unwrap());
    }

    #[test]
    fn shutting_down_counts_as_gone() {
        let state = state_of(&["i-a"]);
        let cloud = vec![inst("i-a", InstanceState::ShuttingDown, None)];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.dropped, vec!["i-a"]);
        assert!(r.live.is_empty() && r.adopted.is_empty());
    }

    #[test]
    fn adopted_instance_with_bad_expires_tag_is_treated_as_expired() {
        let state = state_of(&[]);
        let cloud = vec![inst("i-x", InstanceState::Running, Some("not-a-date"))];
        let r = reconcile(&state, &cloud);
        assert_eq!(r.adopted, vec!["i-x"]);
        assert_eq!(r.live[0].expires_at, chrono::DateTime::UNIX_EPOCH);
    }

    #[test]
    fn empty_everything_reconciles_to_empty() {
        let r = reconcile(&state_of(&[]), &[]);
        assert!(r.live.is_empty() && r.adopted.is_empty() && r.dropped.is_empty());
    }
}
```

- [ ] **Step 2: Run** `cargo test reconcile` — compile failure.

- [ ] **Step 3: Implement** `src/reconcile.rs`:

```rust
use crate::cloud::Instance;
use crate::schema::TAG_EXPIRES;
use crate::state::{InstanceRecord, StateFile};
use chrono::{DateTime, Utc};

pub struct Reconciled {
    pub live: Vec<InstanceRecord>,
    pub adopted: Vec<String>,
    pub dropped: Vec<String>,
}

pub fn reconcile(state: &StateFile, cloud: &[Instance]) -> Reconciled {
    let live_cloud: Vec<&Instance> = cloud.iter().filter(|i| i.state.is_live()).collect();
    let mut live = Vec::new();
    let mut dropped = Vec::new();
    let mut adopted = Vec::new();

    for rec in &state.instances {
        if live_cloud.iter().any(|i| i.id == rec.id) {
            live.push(rec.clone());
        } else {
            dropped.push(rec.id.clone());
        }
    }
    for inst in live_cloud {
        if state.instances.iter().any(|r| r.id == inst.id) {
            continue;
        }
        adopted.push(inst.id.clone());
        let expires_at = inst
            .tags
            .iter()
            .find(|(k, _)| k == TAG_EXPIRES)
            .and_then(|(_, v)| DateTime::parse_from_rfc3339(v).ok())
            .map(|t| t.with_timezone(&Utc))
            // Missing/garbled expiry: treat as already expired so the sweep
            // reaps it, never as trusted.
            .unwrap_or(DateTime::UNIX_EPOCH);
        live.push(InstanceRecord {
            id: inst.id.clone(),
            launched_at: DateTime::UNIX_EPOCH,
            expires_at,
        });
    }
    Reconciled { live, adopted, dropped }
}
```

- [ ] **Step 4: Run** `cargo test reconcile` — pass; clippy clean.

- [ ] **Step 5: Commit** — `feat: adoption reconciliation — cloud tags authoritative, bad expiry means already-expired`.

---

### Task 8: End-of-phase integration test + docs refresh

**Files:**
- Create: `tests/phase1_flow.rs`; Modify: `CLAUDE.md` (Housekeeping → real build/test
  onboarding), `README.md` if stale.

**Interfaces:**
- Consumes: everything above; no new API.

- [ ] **Step 1: Write the flow test** — the abandoned-run story end to end at the library
  level (`tests/phase1_flow.rs`):

```rust
use burst::cloud::fake::FakeCloud;
use burst::cloud::{Cloud, InstanceState, LaunchSpec};
use burst::reconcile::reconcile;
use burst::schema::{RepoId, TagSpec};
use burst::state::{InstanceRecord, RepoState, StateFile};
use chrono::{Duration, Utc};

#[test]
fn abandoned_run_is_adopted_and_reconciled() {
    let repo = RepoId::parse("octo/widgets").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut cloud = FakeCloud::default();

    // Invocation 1: launch 3, record them, then "crash" (drop the lock, keep state).
    let launched = cloud
        .launch(&LaunchSpec {
            count: 3,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec { repo: repo.clone(), expires: Utc::now() + Duration::hours(6) },
            user_data: "jit".into(),
        })
        .unwrap();
    let rs = RepoState::open_at(dir.path().to_path_buf());
    {
        let _lock = rs.lock().unwrap();
        rs.write(&StateFile {
            version: 1,
            repo: repo.to_string(),
            instances: launched
                .iter()
                .map(|i| InstanceRecord {
                    id: i.id.clone(),
                    launched_at: Utc::now(),
                    expires_at: Utc::now() + Duration::hours(6),
                })
                .collect(),
        })
        .unwrap();
    } // lock evaporates; statefile remains — the abandoned-run signal

    // Meanwhile: one VM finishes its job (self-terminates), and another host's
    // instance for the same repo appears.
    cloud.set_state(&launched[0].id, InstanceState::Terminated);
    let stranger = cloud
        .launch(&LaunchSpec {
            count: 1,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec { repo: repo.clone(), expires: Utc::now() + Duration::hours(6) },
            user_data: "jit2".into(),
        })
        .unwrap();

    // Invocation 2: lock is acquirable + statefile present → adopt.
    let _lock = rs.lock().unwrap();
    let state = rs.read().unwrap().expect("abandoned statefile present");
    let live_cloud = cloud.list_tagged(&repo).unwrap();
    let r = reconcile(&state, &live_cloud);
    assert_eq!(r.dropped, vec![launched[0].id.clone()]);
    assert_eq!(r.adopted, vec![stranger[0].id.clone()]);
    assert_eq!(r.live.len(), 3); // 2 survivors + 1 adopted

    // The reconciled manifest is written back atomically.
    rs.write(&StateFile { version: 1, repo: repo.to_string(), instances: r.live }).unwrap();
    assert_eq!(rs.read().unwrap().unwrap().instances.len(), 3);
}
```

- [ ] **Step 2: Run** `cargo test --test phase1_flow` — pass (it composes tested parts; if it
  fails, something above is genuinely wrong — fix there, not here).

- [ ] **Step 3: Docs** — replace CLAUDE.md's Housekeeping build-note with: build `cargo build`,
  test `cargo test`, lint `cargo clippy -- -D warnings`; keep the remote/license notes.

- [ ] **Step 4: Run full suite** `cargo test && cargo clippy -- -D warnings && cargo fmt --check`.

- [ ] **Step 5: Commit** — `test: phase-1 flow test — crash, adoption, reconcile, atomic rewrite; CLAUDE.md build onboarding`.

---

### Task 9: Prove the guard tests' teeth (regression drill)

**Files:** none kept — deliberate temporary regressions, each reverted.

Working norm: "prove a guard test's teeth by regressing the production code." For each
regression below: make the edit, run the named test, confirm it FAILS, `git checkout -- <file>`
(or `git restore`), re-run, confirm PASS. Nothing is committed; afterwards `git status` must be
clean and the full suite green.

- [ ] **Drill 1**: in `state.rs::write`, replace the rename dance with a direct
  `std::fs::write(self.path(), …)` → `write_is_rename_based_so_a_crashed_write_leaves_old_state`
  must still pass (it can't see the difference) BUT `corrupt_statefile_is_a_loud_error_not_empty`
  stays green too — so add the observable: point the direct-write at writing the tmp file
  *without* renaming; now the roundtrip test fails. Record in the report which tests actually
  bit; if any regression survives every test, that's a coverage finding to fix before closing
  the phase (add the missing assertion, then re-drill).
- [ ] **Drill 2**: in `schema.rs::image_key`, drop the length-prefix lines →
  `image_key_fields_are_delimited` must fail.
- [ ] **Drill 3**: in `state.rs::lock`, swap `try_lock` semantics by returning `Ok` on
  `WouldBlock` → `second_lock_fails_fast_while_first_held` must fail.
- [ ] **Drill 4**: in `reconcile.rs`, treat `ShuttingDown` as live (change `is_live`) →
  `shutting_down_counts_as_gone` must fail.
- [ ] **Final**: `git status` clean; `cargo test` green; report which drills bit and any
  coverage gaps found+fixed. Commit only if a drill exposed a gap that required a new test.

---

## Phase gate checklist (from implementation-phases.md)

- [x→verify] Lock: second acquisition fails fast; drop releases; abandoned signal pinned
  (Task 5; binary-level cross-process test deferred to phase 3 — recorded).
- [x→verify] Statefile: write-then-rename survives a modeled crash; corruption is loud (Task 4).
- [x→verify] Image key: stable, input-sensitive, delimited (Task 2).
- [x→verify] Adoption reconciliation vs the fake backend (Tasks 7–8).
- [x→verify] CLI surface + config errors against the compiled binary (Tasks 1, 3).
- [x→verify] Guard-test teeth proven (Task 9).
