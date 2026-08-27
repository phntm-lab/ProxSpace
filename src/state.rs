//! Persistent installation state, stored as `proxspace.state.json`.
//!
//! This one file replaces three separate mechanisms from the original:
//! `setup/installed` (a shell snippet that doubled as the "is it installed?"
//! marker and as storage for the install path), the `MAYBE_FIRST_START`
//! heuristic in `/etc/profile`, and the double shell launch in `runme64.bat`
//! that existed only because `pacman -Syuu` cannot continue in the same
//! process after it replaces the msys2 runtime.
//!
//! Instead the install is an explicit ordered pipeline. Each completed step
//! is recorded, so a run that dies halfway — network drop, power loss, Ctrl+C —
//! resumes from where it stopped rather than starting over or, worse, assuming
//! success.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Format version of the state file. Bumping it requires a migration step;
/// older files are refused rather than silently misread.
pub const SCHEMA_VERSION: u32 = 1;

/// Steps of the install pipeline, in the order they are performed.
///
/// The value stored in the file is the last step that fully completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Stage {
    #[default]
    NotInstalled,
    /// Base archive downloaded and its sha256 verified.
    Downloaded,
    /// Archive unpacked into `msys2/` and deleted.
    Extracted,
    /// First shell run completed, so msys2 initialised its pacman keyring.
    Bootstrapped,
    /// `pacman -Syuu` finished: the msys2 runtime itself is current.
    CoreUpdated,
    /// The ProxSpace package set is installed.
    PackagesInstalled,
    /// Everything done, environment usable.
    Ready,
}

impl Stage {
    /// Position in the pipeline. Ordering is what makes "resume from here"
    /// and "is this a step backwards?" answerable.
    pub fn rank(self) -> u8 {
        match self {
            Stage::NotInstalled => 0,
            Stage::Downloaded => 1,
            Stage::Extracted => 2,
            Stage::Bootstrapped => 3,
            Stage::CoreUpdated => 4,
            Stage::PackagesInstalled => 5,
            Stage::Ready => 6,
        }
    }

    pub fn next(self) -> Option<Stage> {
        match self {
            Stage::NotInstalled => Some(Stage::Downloaded),
            Stage::Downloaded => Some(Stage::Extracted),
            Stage::Extracted => Some(Stage::Bootstrapped),
            Stage::Bootstrapped => Some(Stage::CoreUpdated),
            Stage::CoreUpdated => Some(Stage::PackagesInstalled),
            Stage::PackagesInstalled => Some(Stage::Ready),
            Stage::Ready => None,
        }
    }

    /// Forward movement is one step at a time — skipping a step would mean
    /// claiming work that never ran. Backward movement of any distance is
    /// allowed: that is how a failure, a wipe or a forced reinstall is recorded.
    pub fn may_move_to(self, target: Stage) -> bool {
        target.rank() <= self.rank() || target.rank() == self.rank() + 1
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Stage::NotInstalled => "not installed",
            Stage::Downloaded => "archive downloaded",
            Stage::Extracted => "archive extracted",
            Stage::Bootstrapped => "msys2 bootstrapped",
            Stage::CoreUpdated => "msys2 core updated",
            Stage::PackagesInstalled => "packages installed",
            Stage::Ready => "ready",
        };
        f.write_str(name)
    }
}

/// Which base archive the tree in `msys2/` came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msys2Info {
    /// Datestamp from the archive name, e.g. `20260611`.
    pub version: String,
    pub source_url: String,
    pub sha256: String,
    pub extracted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagesInfo {
    pub installed_at: String,
    /// Hash of the package list the install was made from. A mismatch means a
    /// newer binary ships a different list and the delta must be installed —
    /// without reinstalling everything.
    pub list_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub schema: u32,
    pub msys2: Option<Msys2Info>,
    /// Where the environment was installed. Compared against the current base
    /// directory to detect a moved folder — the original did the same by
    /// comparing `PSINSTALLPATH` with `OLDPWD`, because msys2 bakes absolute
    /// paths into its packages and a move breaks them.
    pub install_path: Option<String>,
    /// Version of the binary that last wrote this file.
    pub proxspace_version: String,
    pub stage: Stage,
    pub packages: Option<PackagesInfo>,
    pub pip_extras_installed: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            schema: SCHEMA_VERSION,
            msys2: None,
            install_path: None,
            proxspace_version: env!("CARGO_PKG_VERSION").to_string(),
            stage: Stage::NotInstalled,
            packages: None,
            pip_extras_installed: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("cannot write the state file `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot serialise the state")]
    Serialise(#[source] serde_json::Error),
    #[error("refusing to move the install state from `{from}` to `{to}`")]
    IllegalTransition { from: Stage, to: Stage },
}

/// Outcome of reading the state file. Reading never fails: a missing or
/// unreadable file means "start from scratch", which is always a safe reading —
/// every install step is idempotent. What must not happen is silently losing
/// the reason, hence the warning.
#[derive(Debug)]
pub struct LoadOutcome {
    pub state: State,
    /// Set when the file existed but could not be used as-is.
    pub warning: Option<String>,
}

impl State {
    pub fn load(path: &Path) -> LoadOutcome {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return LoadOutcome {
                    state: State::default(),
                    warning: None,
                };
            }
            Err(error) => {
                return LoadOutcome {
                    state: State::default(),
                    warning: Some(format!(
                        "cannot read `{}` ({error}); assuming a fresh install",
                        path.display()
                    )),
                };
            }
        };

        match serde_json::from_str::<State>(&text) {
            Ok(state) if state.schema == SCHEMA_VERSION => LoadOutcome {
                state,
                warning: None,
            },
            Ok(state) if state.schema > SCHEMA_VERSION => LoadOutcome {
                warning: Some(format!(
                    "`{}` was written by a newer ProxSpace (state format {}, this build understands {}); \
                     starting from scratch would delete a working install, so nothing is assumed — \
                     use a matching ProxSpace build",
                    path.display(),
                    state.schema,
                    SCHEMA_VERSION
                )),
                state,
            },
            Ok(state) => LoadOutcome {
                warning: Some(format!(
                    "`{}` uses the older state format {} and no migration exists yet; \
                     assuming a fresh install",
                    path.display(),
                    state.schema
                )),
                state: State::default(),
            },
            Err(error) => LoadOutcome {
                state: State::default(),
                warning: Some(format!(
                    "`{}` is not valid state JSON ({error}); assuming a fresh install",
                    path.display()
                )),
            },
        }
    }

    /// Write the state so that a crash mid-write cannot corrupt it: serialise
    /// in full, flush to a sibling temporary file, then rename over the target.
    /// On Windows `rename` maps to `MoveFileEx(..., MOVEFILE_REPLACE_EXISTING)`,
    /// which replaces the file in a single metadata operation.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let mut json = serde_json::to_string_pretty(self).map_err(StateError::Serialise)?;
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

    /// Record that a pipeline step completed, or that the install was rolled
    /// back to an earlier point.
    pub fn move_to(&mut self, target: Stage) -> Result<(), StateError> {
        if !self.stage.may_move_to(target) {
            return Err(StateError::IllegalTransition {
                from: self.stage,
                to: target,
            });
        }
        self.stage = target;
        Ok(())
    }

    /// True when the environment was installed somewhere else and the folder
    /// has since been moved or copied.
    pub fn was_moved_from(&self, current_base: &Path) -> bool {
        match &self.install_path {
            Some(recorded) => !paths_equal(Path::new(recorded), current_base),
            None => false,
        }
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

/// Current time as an RFC 3339 UTC timestamp, the format used for every
/// timestamp stored in the state file.
pub fn timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> State {
        State {
            schema: SCHEMA_VERSION,
            msys2: Some(Msys2Info {
                version: "20260611".to_string(),
                source_url:
                    "https://mirror.msys2.org/distrib/x86_64/msys2-base-x86_64-20260611.tar.xz"
                        .to_string(),
                sha256: "0".repeat(64),
                extracted_at: "2026-08-27T10:00:00Z".to_string(),
            }),
            install_path: Some(r"C:\ProxSpace".to_string()),
            proxspace_version: "0.1.0".to_string(),
            stage: Stage::Ready,
            packages: Some(PackagesInfo {
                installed_at: "2026-08-27T10:20:00Z".to_string(),
                list_hash: "a".repeat(64),
            }),
            pip_extras_installed: true,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let state = populated();
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), state);
    }

    #[test]
    fn stage_is_stored_by_name() {
        let json = serde_json::to_string(&populated()).unwrap();
        assert!(
            json.contains(r#""stage":"Ready""#),
            "unexpected json: {json}"
        );
    }

    #[test]
    fn stages_are_ordered() {
        assert!(Stage::NotInstalled < Stage::Downloaded);
        assert!(Stage::PackagesInstalled < Stage::Ready);
        assert_eq!(Stage::Downloaded.next(), Some(Stage::Extracted));
        assert_eq!(Stage::Ready.next(), None);
    }

    #[test]
    fn one_step_forward_is_allowed() {
        let mut state = State::default();
        for stage in [
            Stage::Downloaded,
            Stage::Extracted,
            Stage::Bootstrapped,
            Stage::CoreUpdated,
            Stage::PackagesInstalled,
            Stage::Ready,
        ] {
            state.move_to(stage).unwrap();
            assert_eq!(state.stage, stage);
        }
    }

    #[test]
    fn skipping_a_step_is_rejected() {
        let mut state = State::default();
        assert!(matches!(
            state.move_to(Stage::Extracted),
            Err(StateError::IllegalTransition {
                from: Stage::NotInstalled,
                to: Stage::Extracted
            })
        ));
        assert_eq!(state.stage, Stage::NotInstalled);
    }

    #[test]
    fn rolling_back_any_distance_is_allowed() {
        let mut state = populated();
        state.move_to(Stage::NotInstalled).unwrap();
        assert_eq!(state.stage, Stage::NotInstalled);
    }

    #[test]
    fn staying_put_is_allowed() {
        let mut state = populated();
        state.move_to(Stage::Ready).unwrap();
        assert_eq!(state.stage, Stage::Ready);
    }

    #[test]
    fn a_move_is_detected_case_insensitively() {
        let state = populated();
        assert!(!state.was_moved_from(Path::new(r"c:\proxspace")));
        assert!(!state.was_moved_from(Path::new(r"C:\ProxSpace\")));
        assert!(state.was_moved_from(Path::new(r"D:\ProxSpace")));
    }

    #[test]
    fn a_fresh_install_is_never_reported_as_moved() {
        assert!(!State::default().was_moved_from(Path::new(r"C:\ProxSpace")));
    }

    #[test]
    fn a_directory_that_merely_starts_the_same_is_a_different_directory() {
        let state = populated();
        assert!(state.was_moved_from(Path::new(r"C:\ProxSpace2")));
        assert!(state.was_moved_from(Path::new(r"C:\ProxSpace\msys2")));
        assert!(state.was_moved_from(Path::new(r"C:\Prox")));
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
        assert!(!state.was_moved_from(&here.join(".").join(".")));
        assert!(state.was_moved_from(&dir.path().join("elsewhere")));
    }
}
