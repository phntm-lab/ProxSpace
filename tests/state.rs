//! Lifecycle of `proxspace.state.json` on a real filesystem.
//!
//! The state file is what makes a failed install resumable instead of
//! restartable, so the cases that matter are the ugly ones: the file is
//! missing, truncated, garbage, or written by a different version.

use std::fs;

use proxspace::core::state::{Msys2Info, PackagesInfo, SCHEMA_VERSION, Stage, State, timestamp};
use proxspace::infra::state as state_file;
use tempfile::TempDir;

fn base() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn installed_state(install_path: &str) -> State {
    State {
        schema: SCHEMA_VERSION,
        msys2: Some(Msys2Info {
            version: "20260611".to_string(),
            source_url: "https://mirror.msys2.org/distrib/x86_64/msys2-base-x86_64-20260611.tar.xz"
                .to_string(),
            sha256: "0".repeat(64),
            extracted_at: timestamp(),
        }),
        install_path: Some(install_path.to_string()),
        proxspace_version: proxspace::VERSION.to_string(),
        stage: Stage::Ready,
        packages: Some(PackagesInfo {
            installed_at: timestamp(),
            list_hash: "b".repeat(64),
        }),
        pip_extras_installed: true,
    }
}

#[test]
fn a_missing_file_reads_as_a_fresh_install() {
    let dir = base();
    let loaded = state_file::load(&dir.path().join("proxspace.state.json"));

    assert_eq!(loaded.state.stage, Stage::NotInstalled);
    // Nothing went wrong, so nothing should be reported to the user.
    assert!(loaded.warning.is_none());
}

#[test]
fn what_is_written_is_what_is_read_back() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let state = installed_state(r"C:\ProxSpace");

    state_file::save(&state, &path).unwrap();
    let loaded = state_file::load(&path);

    assert!(loaded.warning.is_none());
    assert_eq!(loaded.state, state);
}

#[test]
fn saving_leaves_no_temporary_file_behind() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    state_file::save(&installed_state(r"C:\ProxSpace"), &path).unwrap();

    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(leftovers, ["proxspace.state.json"]);
}

#[test]
fn a_second_save_replaces_the_first() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");

    state_file::save(&installed_state(r"C:\First"), &path).unwrap();
    state_file::save(&installed_state(r"C:\Second"), &path).unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.state.install_path.as_deref(), Some(r"C:\Second"));
}

#[test]
fn garbage_is_reported_and_treated_as_a_fresh_install() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    fs::write(&path, "{ not json at all").unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.state.stage, Stage::NotInstalled);
    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("not valid state JSON")),
        "unexpected warning: {:?}",
        loaded.warning
    );
}

#[test]
fn a_truncated_file_is_reported_and_treated_as_a_fresh_install() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let full = serde_json::to_string_pretty(&installed_state(r"C:\ProxSpace")).unwrap();
    fs::write(&path, &full[..full.len() / 2]).unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.state.stage, Stage::NotInstalled);
    assert!(loaded.warning.is_some());
}

#[test]
fn a_state_file_in_an_unreachable_older_format_is_reported_and_started_over() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let mut value: serde_json::Value =
        serde_json::to_value(installed_state(r"C:\ProxSpace")).unwrap();
    // No build ever wrote format 0, so no rung leads out of it; a real older
    // format gets one, and this stays the shape of the failure when it does not.
    value["schema"] = serde_json::json!(0);
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.state.stage, Stage::NotInstalled);
    assert_eq!(loaded.migrated_from, None);
    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("older state format 0")),
        "unexpected warning: {:?}",
        loaded.warning
    );
}

#[test]
fn a_state_file_without_a_format_is_reported_and_started_over() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let mut value: serde_json::Value =
        serde_json::to_value(installed_state(r"C:\ProxSpace")).unwrap();
    value.as_object_mut().unwrap().remove("schema");
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.state.stage, Stage::NotInstalled);
    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("does not say which state format")),
        "unexpected warning: {:?}",
        loaded.warning
    );
}

#[test]
fn a_file_in_the_current_format_is_not_migrated() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    state_file::save(&installed_state(r"C:\ProxSpace"), &path).unwrap();

    let loaded = state_file::load(&path);
    assert_eq!(loaded.migrated_from, None);
    assert!(loaded.warning.is_none());
}

#[test]
fn a_state_file_from_a_newer_binary_is_not_silently_discarded() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let mut value: serde_json::Value =
        serde_json::to_value(installed_state(r"C:\ProxSpace")).unwrap();
    value["schema"] = serde_json::json!(SCHEMA_VERSION + 1);
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let loaded = state_file::load(&path);
    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("newer ProxSpace")),
        "unexpected warning: {:?}",
        loaded.warning
    );
    // Reporting it is the point; wiping a working install would not be.
    assert_eq!(loaded.state.stage, Stage::Ready);
}

#[test]
fn an_interrupted_install_resumes_from_the_last_completed_step() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");

    // First run gets as far as unpacking the archive, then dies.
    let mut state = State::default();
    state.move_to(Stage::Downloaded).unwrap();
    state_file::save(&state, &path).unwrap();
    state.move_to(Stage::Extracted).unwrap();
    state_file::save(&state, &path).unwrap();

    // Second run picks the pipeline up where it stopped.
    let mut resumed = state_file::load(&path).state;
    assert_eq!(resumed.stage, Stage::Extracted);
    assert_eq!(resumed.stage.next(), Some(Stage::Bootstrapped));

    resumed.move_to(Stage::Bootstrapped).unwrap();
    state_file::save(&resumed, &path).unwrap();
    assert_eq!(state_file::load(&path).state.stage, Stage::Bootstrapped);
}

#[test]
fn a_wipe_resets_the_pipeline_to_the_beginning() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    let mut state = installed_state(r"C:\ProxSpace");

    state.move_to(Stage::NotInstalled).unwrap();
    state_file::save(&state, &path).unwrap();

    assert_eq!(state_file::load(&path).state.stage, Stage::NotInstalled);
}

#[test]
fn a_moved_installation_is_detected_across_a_save() {
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    state_file::save(&installed_state(r"C:\ProxSpace"), &path).unwrap();

    let state = state_file::load(&path).state;
    assert!(state_file::was_moved_from(
        &state,
        std::path::Path::new(r"D:\Somewhere\Else")
    ));
    assert!(!state_file::was_moved_from(
        &state,
        std::path::Path::new(r"C:\ProxSpace")
    ));
}

#[test]
fn the_file_is_readable_json() {
    // The state file doubles as a diagnostic the user can open in an editor,
    // so it is pretty-printed rather than minified.
    let dir = base();
    let path = dir.path().join("proxspace.state.json");
    state_file::save(&installed_state(r"C:\ProxSpace"), &path).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains('\n'), "state file should be pretty-printed");
    assert!(text.ends_with('\n'), "state file should end with a newline");
}
