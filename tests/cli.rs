use assert_cmd::Command;
use predicates::prelude::*;

fn burst() -> Command {
    Command::cargo_bin("burst").unwrap()
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
