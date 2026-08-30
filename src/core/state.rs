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

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Format version of the state file. Bumping it requires a matching rung in
/// [`MIGRATIONS`]; files that cannot be brought forward are refused rather than
/// silently misread.
pub const SCHEMA_VERSION: u32 = 1;

/// One rung of the migration ladder: it rewrites a state file written with
/// schema `from` into the shape schema `from + 1` reads.
///
/// Rungs work on raw JSON rather than on [`State`], because the old shape is by
/// definition no longer the shape [`State`] deserialises. Setting the `schema`
/// field is not a rung's job — [`climb`] does that after each successful step.
struct Migration {
    from: u32,
    apply: fn(&mut serde_json::Value) -> Result<(), String>,
}

/// The ladder, in order, one rung per bump of [`SCHEMA_VERSION`]: adding a
/// field with its default, renaming one, dropping one. A rung is what keeps an
/// existing install from being thrown away and reinstalled after a format
/// change, so a bump without one is a mistake — `the_ladder_is_unbroken`
/// catches it.
///
/// Empty while schema 1 is the only format that has ever been written.
const MIGRATIONS: &[Migration] = &[];

/// Bring raw state JSON from schema `from` up to schema `to`, one rung at a
/// time. `to` is a parameter rather than [`SCHEMA_VERSION`] so the climb can be
/// exercised over a ladder that is not the shipped one.
fn climb(
    value: &mut serde_json::Value,
    from: u32,
    to: u32,
    ladder: &[Migration],
) -> Result<(), String> {
    if !value.is_object() {
        return Err("the file does not contain a JSON object".to_string());
    }

    let mut schema = from;
    while schema < to {
        let rung = ladder
            .iter()
            .find(|migration| migration.from == schema)
            .ok_or_else(|| format!("this build knows no way forward from format {schema}"))?;
        (rung.apply)(value).map_err(|reason| format!("format {schema}: {reason}"))?;
        schema += 1;
        value["schema"] = serde_json::json!(schema);
    }
    Ok(())
}

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
    /// Format the file was written in, when it had to be brought forward to
    /// the current one. The upgraded state is not written back here; whichever
    /// command saves next stores it in the new format, and until then the file
    /// stays readable by the build that wrote it.
    pub migrated_from: Option<u32>,
}

impl LoadOutcome {
    /// A file that cannot be used: every install step is idempotent, so
    /// starting over is always safe, but the reason must not be swallowed.
    pub(crate) fn fresh(warning: String) -> LoadOutcome {
        LoadOutcome {
            state: State::default(),
            warning: Some(warning),
            migrated_from: None,
        }
    }
}

impl State {
    /// Read state JSON: check the format, bring an older one forward, and
    /// turn what cannot be used into a warning rather than an error.
    ///
    /// `source` names the file the text came from and appears in every
    /// warning; the text itself arrives already read.
    pub fn parse(text: &str, source: &str) -> LoadOutcome {
        let mut value = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) => value,
            Err(error) => {
                return LoadOutcome::fresh(format!(
                    "`{}` is not valid state JSON ({error}); assuming a fresh install",
                    source
                ));
            }
        };

        let schema = match value.get("schema").and_then(serde_json::Value::as_u64) {
            Some(schema) => u32::try_from(schema).unwrap_or(u32::MAX),
            None => {
                return LoadOutcome::fresh(format!(
                    "`{}` does not say which state format it is in; assuming a fresh install",
                    source
                ));
            }
        };

        if schema > SCHEMA_VERSION {
            let warning = format!(
                "`{}` was written by a newer ProxSpace (state format {schema}, this build \
                 understands {SCHEMA_VERSION}); starting from scratch would delete a working \
                 install, so nothing is assumed — use a matching ProxSpace build",
                source
            );
            // Keeping whatever of it can be read beats reporting a working
            // install as absent.
            return match serde_json::from_value::<State>(value) {
                Ok(state) => LoadOutcome {
                    state,
                    warning: Some(warning),
                    migrated_from: None,
                },
                Err(_) => LoadOutcome::fresh(warning),
            };
        }

        let mut migrated_from = None;
        if schema < SCHEMA_VERSION {
            if let Err(reason) = climb(&mut value, schema, SCHEMA_VERSION, MIGRATIONS) {
                return LoadOutcome::fresh(format!(
                    "`{}` is in the older state format {schema} and cannot be brought forward \
                     ({reason}); assuming a fresh install",
                    source
                ));
            }
            migrated_from = Some(schema);
        }

        match serde_json::from_value::<State>(value) {
            Ok(state) => LoadOutcome {
                state,
                warning: None,
                migrated_from,
            },
            Err(error) => LoadOutcome::fresh(match migrated_from {
                Some(from) => format!(
                    "`{}` was brought forward from state format {from} into something this build \
                     cannot read ({error}); assuming a fresh install",
                    source
                ),
                None => format!(
                    "`{}` is not valid state JSON ({error}); assuming a fresh install",
                    source
                ),
            }),
        }
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

    /// Forget everything that described an msys2 tree that is no longer on
    /// disk, and walk the pipeline back to the beginning.
    ///
    /// Everything recorded here is about the contents of that tree — which
    /// packages went in, whether the python extras were added — so a tree that
    /// has been deleted takes all of it with it. Saving is left to the caller,
    /// which usually has more to record in the same write.
    pub fn forget_msys2(&mut self) -> Result<(), StateError> {
        self.msys2 = None;
        self.packages = None;
        self.pip_extras_installed = false;
        self.move_to(Stage::NotInstalled)
    }
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

    /// A ladder is only useful if it has no gaps: every format from the oldest
    /// rung up to the current one must have a way forward.
    #[test]
    fn the_ladder_is_unbroken() {
        let Some(first) = MIGRATIONS.first() else {
            return;
        };
        for (offset, rung) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                rung.from,
                first.from + offset as u32,
                "the migration ladder skips a state format"
            );
        }
        assert_eq!(
            MIGRATIONS.last().map(|rung| rung.from + 1),
            Some(SCHEMA_VERSION),
            "the migration ladder does not reach the current state format"
        );
    }

    #[test]
    fn climbing_applies_every_rung_in_order_and_stamps_the_schema() {
        let ladder = [
            Migration {
                from: 1,
                apply: |value| {
                    value["added"] = serde_json::json!("by the first rung");
                    Ok(())
                },
            },
            Migration {
                from: 2,
                apply: |value| {
                    let old = value["added"].as_str().unwrap_or_default().to_string();
                    value["added"] = serde_json::json!(format!("{old}, then the second"));
                    Ok(())
                },
            },
        ];

        let mut value = serde_json::json!({ "schema": 1 });
        climb(&mut value, 1, 3, &ladder).unwrap();

        assert_eq!(value["schema"], serde_json::json!(3));
        assert_eq!(value["added"], "by the first rung, then the second");
    }

    #[test]
    fn climbing_nowhere_changes_nothing() {
        let mut value = serde_json::json!({ "schema": 1, "stage": "Ready" });
        let before = value.clone();
        climb(&mut value, 1, 1, &[]).unwrap();
        assert_eq!(value, before);
    }

    #[test]
    fn a_missing_rung_stops_the_climb_where_it_stands() {
        let ladder = [Migration {
            from: 1,
            apply: |value| {
                value["reached"] = serde_json::json!(2);
                Ok(())
            },
        }];

        let mut value = serde_json::json!({ "schema": 1 });
        let reason = climb(&mut value, 1, 4, &ladder).unwrap_err();

        assert!(reason.contains("from format 2"), "unexpected: {reason}");
        // The rungs that did run are not undone; the file is discarded whole.
        assert_eq!(value["schema"], serde_json::json!(2));
    }

    #[test]
    fn a_failing_rung_names_the_format_it_failed_on() {
        let ladder = [
            Migration {
                from: 1,
                apply: |_| Ok(()),
            },
            Migration {
                from: 2,
                apply: |_| Err("the field is not a string".to_string()),
            },
        ];

        let mut value = serde_json::json!({ "schema": 1 });
        let reason = climb(&mut value, 1, 3, &ladder).unwrap_err();

        assert_eq!(reason, "format 2: the field is not a string");
    }

    #[test]
    fn climbing_refuses_json_that_is_not_an_object() {
        let mut value = serde_json::json!([1, 2, 3]);
        assert!(climb(&mut value, 1, 2, &[]).is_err());
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
    fn forgetting_the_tree_forgets_what_was_in_it() {
        let mut state = populated();
        state.forget_msys2().unwrap();

        assert_eq!(state.stage, Stage::NotInstalled);
        assert_eq!(state.msys2, None);
        assert_eq!(state.packages, None);
        assert!(!state.pip_extras_installed);
        // Where it is installed is about the folder, not about the tree.
        assert_eq!(state.install_path.as_deref(), Some(r"C:\ProxSpace"));
    }
}
