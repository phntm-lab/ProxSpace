//! End-to-end behaviour of the command line: exit codes, help output and the
//! checks that run before any command does its work.

use std::fs;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// Exit code clap uses for usage errors.
const EXIT_USAGE: i32 = 2;
/// Exit code for a subcommand that is not implemented yet.
const EXIT_NOT_IMPLEMENTED: i32 = 3;

fn proxspace() -> Command {
    Command::cargo_bin("proxspace").expect("the proxspace binary should be built")
}

/// A base directory the binary is pointed at with `--dir`, so that tests never
/// write next to the real executable.
fn base() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn in_dir(dir: &Path) -> Command {
    let mut command = proxspace();
    command.arg("--dir").arg(dir).arg("--no-color");
    command
}

#[test]
fn help_lists_every_command() {
    proxspace()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("shell"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("repair"))
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("mirrors"))
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("autobuild"))
        .stdout(predicate::str::contains("clean"));
}

#[test]
fn version_is_reported() {
    proxspace()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    proxspace()
        .arg("definitely-not-a-command")
        .assert()
        .code(EXIT_USAGE);
}

#[test]
fn help_does_not_create_any_files() {
    let dir = base();
    in_dir(dir.path()).arg("--help").assert().success();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn info_works_on_an_empty_directory() {
    let dir = base();
    in_dir(dir.path())
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("proxspace"))
        .stdout(predicate::str::contains("version"))
        // Nothing is installed and every section says so rather than failing.
        .stdout(predicate::str::contains("not installed"))
        .stdout(predicate::str::contains("toolchain"))
        .stdout(predicate::str::contains("system"));
}

/// The report is meant to be attached to a bug report, so it has to exist as a
/// file and hold the same text that was printed.
#[test]
fn info_leaves_its_report_next_to_the_binary() {
    let dir = base();
    let printed = in_dir(dir.path()).arg("info").assert().success();
    let printed = String::from_utf8(printed.get_output().stdout.clone()).unwrap();

    let written = fs::read_to_string(dir.path().join("proxspace-info.txt")).unwrap();
    assert_eq!(written, printed);
}

#[test]
fn every_run_writes_a_log() {
    let dir = base();
    in_dir(dir.path()).arg("info").assert().success();

    let log = fs::read_to_string(dir.path().join("proxspace.log")).unwrap();
    assert!(log.contains("starting: info"), "unexpected log: {log}");
}

#[test]
fn unimplemented_commands_say_so_and_use_their_own_exit_code() {
    let dir = base();
    // Only `autobuild` is left: everything else does the real thing, and what
    // it does is checked against a fake environment in `install_flow.rs`.
    in_dir(dir.path())
        .arg("autobuild")
        .assert()
        .code(EXIT_NOT_IMPLEMENTED)
        .stderr(predicate::str::contains("not implemented yet"));
}

/// A run with no arguments is the `runme64.bat` case and must dispatch to
/// `shell`. Checked on a path the environment cannot use, so that the run stops
/// at preflight instead of going on to provision an msys2 tree.
#[test]
fn no_arguments_means_shell() {
    let parent = base();
    let spaced = parent.path().join("Program Files");
    fs::create_dir(&spaced).unwrap();

    in_dir(&spaced).assert().code(1);

    let log = fs::read_to_string(spaced.join("proxspace.log")).unwrap();
    assert!(log.contains("starting: shell"), "unexpected log: {log}");
}

#[test]
fn an_install_path_with_a_space_is_refused_with_a_specific_reason() {
    let parent = base();
    let spaced = parent.path().join("Program Files");
    fs::create_dir(&spaced).unwrap();

    in_dir(&spaced)
        .arg("shell")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("contains a space"));
}

#[test]
fn an_install_path_with_non_ascii_is_refused() {
    let parent = base();
    let cyrillic = parent.path().join("Проекты");
    fs::create_dir(&cyrillic).unwrap();

    in_dir(&cyrillic)
        .arg("shell")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("non-ASCII"));
}

#[test]
fn info_still_works_on_a_path_the_environment_could_not_use() {
    // Diagnosing a bad path is exactly when `info` has to keep working.
    let parent = base();
    let spaced = parent.path().join("bad path");
    fs::create_dir(&spaced).unwrap();

    in_dir(&spaced).arg("info").assert().success();
}

#[test]
fn a_missing_dir_is_reported_clearly() {
    let dir = base();
    let missing = dir.path().join("nowhere");

    proxspace()
        .arg("--dir")
        .arg(&missing)
        .arg("info")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn a_corrupt_state_file_is_reported_but_not_fatal() {
    let dir = base();
    fs::write(
        dir.path().join("proxspace.state.json"),
        "{ this is not json",
    )
    .unwrap();

    in_dir(dir.path())
        .arg("info")
        .assert()
        .success()
        .stderr(predicate::str::contains("not valid state JSON"));
}

#[test]
fn quiet_hides_progress_but_keeps_command_output() {
    let dir = base();
    in_dir(dir.path())
        .args(["--quiet", "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("proxspace"));
}

#[test]
fn quiet_and_verbose_cannot_be_combined() {
    proxspace()
        .args(["--quiet", "--verbose", "info"])
        .assert()
        .code(EXIT_USAGE);
}

#[test]
fn exec_without_a_command_is_a_usage_error() {
    proxspace().arg("exec").assert().code(EXIT_USAGE);
}

#[test]
fn global_flags_are_accepted_after_the_subcommand() {
    let dir = base();
    in_dir(dir.path())
        .args(["info", "--verbose"])
        .assert()
        .success()
        .stdout(predicate::str::contains("base directory:"));
}

/// A repair has nothing to work on until msys2 is unpacked, and must say which
/// directory it was looking for rather than failing somewhere inside pacman.
#[test]
fn repairing_an_empty_directory_says_the_tree_is_not_there() {
    let dir = base();
    in_dir(dir.path())
        .arg("repair")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("has not been unpacked"));
}

/// `clean` with nothing installed must say so rather than reporting a failure
/// from inside pacman.
#[test]
fn cleaning_an_empty_directory_says_there_is_nothing_to_clean() {
    let dir = base();
    in_dir(dir.path())
        .arg("clean")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("nothing to clean"));
}

/// Both halves of `mirrors` reach their command rather than falling through to
/// the not-implemented branch.
#[test]
fn mirror_commands_need_an_unpacked_tree() {
    let dir = base();
    for action in ["rank", "restore"] {
        in_dir(dir.path())
            .args(["mirrors", action])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("has not been unpacked"));
    }
}
