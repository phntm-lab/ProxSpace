//! The state file on disk: reading it, writing it, and telling whether the
//! folder it describes is still the folder we are running from.
//!
//! Everything the contents mean lives in [`crate::core::state`]; this is only
//! the part that touches the filesystem.

use std::fs;
use std::io;
use std::path::Path;

use crate::core::state::{LoadOutcome, State, StateError};

/// Read the state file. Reading never fails: a missing or unreadable file
/// means "start from scratch", which is always safe because every install step
/// is idempotent. What must not happen is silently losing the reason.
pub fn load(path: &Path) -> LoadOutcome {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return LoadOutcome {
                state: State::default(),
                warning: None,
                migrated_from: None,
            };
        }
        Err(error) => {
            return LoadOutcome::fresh(format!(
                "cannot read `{}` ({error}); assuming a fresh install",
                path.display()
            ));
        }
    };

    State::parse(&text, &path.display().to_string())
}

/// Write the state so that a crash mid-write cannot corrupt it: serialise
/// in full, flush to a sibling temporary file, then rename over the target.
/// On Windows `rename` maps to `MoveFileEx(..., MOVEFILE_REPLACE_EXISTING)`,
/// which replaces the file in a single metadata operation.
pub fn save(state: &State, path: &Path) -> Result<(), StateError> {
    let mut json = serde_json::to_string_pretty(state).map_err(StateError::Serialise)?;
    json.push('\n');

    let temporary = path.with_extension("json.tmp");
    write_and_sync(&temporary, json.as_bytes()).map_err(|source| StateError::Write {
        path: temporary.clone(),
        source,
    })?;

    fs::rename(&temporary, path).map_err(|source| {
        let _ = fs::remove_file(&temporary);
        StateError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// True when the environment was installed somewhere else and the folder has
/// since been moved or copied.
pub fn was_moved_from(state: &State, current_base: &Path) -> bool {
    match &state.install_path {
        Some(recorded) => !paths_equal(Path::new(recorded), current_base),
        None => false,
    }
}

/// Compare install paths the way Windows does: case-insensitively, and without
/// caring whether a trailing separator is present.
///
/// When both paths exist the filesystem answers instead, which also settles
/// short names, symbolic links and `.` components. The recorded path usually
/// does not exist any more — that is the situation being detected — so the
/// textual comparison is what normally decides.
fn paths_equal(a: &Path, b: &Path) -> bool {
    fn normalise(path: &Path) -> String {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
            .replace('/', "\\")
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return a == b;
    }
    normalise(a) == normalise(b)
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    // Without this the rename can land before the data does, leaving a
    // zero-length state file after an unclean shutdown.
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::state::Stage;

    /// A state file describing a finished install in `C:\ProxSpace`.
    fn populated() -> State {
        State {
            stage: Stage::Ready,
            install_path: Some(r"C:\ProxSpace".to_string()),
            ..State::default()
        }
    }

    #[test]
    fn a_move_is_detected_case_insensitively() {
        let state = populated();
        assert!(!was_moved_from(&state, Path::new(r"c:\proxspace")));
        assert!(!was_moved_from(&state, Path::new(r"C:\ProxSpace\")));
        assert!(was_moved_from(&state, Path::new(r"D:\ProxSpace")));
    }

    #[test]
    fn a_fresh_install_is_never_reported_as_moved() {
        assert!(!was_moved_from(
            &State::default(),
            Path::new(r"C:\ProxSpace")
        ));
    }

    #[test]
    fn a_directory_that_merely_starts_the_same_is_a_different_directory() {
        let state = populated();
        assert!(was_moved_from(&state, Path::new(r"C:\ProxSpace2")));
        assert!(was_moved_from(&state, Path::new(r"C:\ProxSpace\msys2")));
        assert!(was_moved_from(&state, Path::new(r"C:\Prox")));
    }

    #[test]
    fn two_spellings_of_one_real_directory_are_not_a_move() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("install");
        fs::create_dir(&here).unwrap();

        let state = State {
            install_path: Some(here.to_string_lossy().into_owned()),
            ..State::default()
        };

        // The same directory written a longer way round: only the filesystem
        // can tell that these are one place.
        assert!(!was_moved_from(&state, &here.join(".").join(".")));
        assert!(was_moved_from(&state, &dir.path().join("elsewhere")));
    }
}
