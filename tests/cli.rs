use assert_cmd::Command;
use predicates::prelude::*;
use std::sync::LazyLock;
use tempfile::TempDir;

static SCRATCH: LazyLock<TempDir> = LazyLock::new(|| tempfile::tempdir().unwrap());

fn burst() -> Command {
    let mut cmd = Command::cargo_bin("burst").unwrap();
    cmd.current_dir(SCRATCH.path());
    cmd
}

#[test]
fn no_args_prints_usage() {
    burst()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage"));
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
fn down_fails_loud_offline_not_silently() {
    // Like status, down's first step is a bare AWS list — no GitHub call
    // yet — so (as in status_fails_loud_offline_not_silently) only killing
    // every region source forces a fast, local, no-real-AWS-touched
    // failure; this host's ~/.aws/credentials would otherwise resolve real
    // credentials and down would truthfully report an empty fleet instead
    // of failing.
    burst()
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env("AWS_CONFIG_FILE", "/nonexistent/config")
        .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/credentials")
        .args(["down", "--repo", "octo/widgets"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn status_fails_loud_offline_not_silently() {
    // Status never calls GitHub, so (unlike bake/sweep) env_remove-ing
    // GitHub credentials can't force an early, network-free failure — and
    // this host's ~/.aws/credentials carries real AWS keys, so merely
    // removing the AWS_* env vars still resolves credentials via that file.
    // Point the SDK's config/credentials files at nowhere and strip every
    // region source instead: region resolution fails locally, before any
    // network call, giving a fast, no-real-AWS-touched, fail-loud check.
    burst()
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .env("AWS_CONFIG_FILE", "/nonexistent/config")
        .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/credentials")
        .args(["status", "--repo", "octo/widgets"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn sweep_fails_loud_offline_not_silently() {
    // No credentials, no GitHub token: sweep now does real work instead of
    // `not_implemented`, but must still fail loud — exit 1, a clear
    // stderr reason — rather than silently doing nothing.
    burst()
        .env_remove("BURST_GITHUB_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .args(["sweep", "--repo", "octo/widgets"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn bake_fails_loud_offline_not_silently() {
    // No credentials, no GitHub token: bake now does real work instead of
    // `not_implemented`, but must still fail loud — exit 1, a clear
    // stderr reason — rather than silently doing nothing.
    burst()
        .env_remove("BURST_GITHUB_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .args(["bake", "--repo", "octo/widgets"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("error:"));
}

#[test]
fn missing_repo_fails_with_remedy() {
    let d = tempfile::tempdir().unwrap();
    Command::cargo_bin("burst")
        .unwrap()
        .current_dir(d.path())
        .args(["status"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--repo"));
}

#[test]
fn unknown_config_key_fails_loud() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("burst.toml"),
        "[burst]\nrepo=\"a/b\"\nbogus=1\n",
    )
    .unwrap();
    Command::cargo_bin("burst")
        .unwrap()
        .current_dir(d.path())
        .args(["status"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("bogus"));
}
