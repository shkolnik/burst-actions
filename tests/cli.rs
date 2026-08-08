use assert_cmd::Command;
use predicates::prelude::*;

fn burst() -> Command {
    let mut cmd = Command::cargo_bin("burst").unwrap();
    cmd.current_dir(tempfile::tempdir().unwrap().keep());
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
fn subcommands_fail_loud_not_silent() {
    for cmd in ["bake", "status", "sweep"] {
        burst()
            .args([cmd, "--repo", "octo/widgets"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("not implemented yet"));
    }
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
