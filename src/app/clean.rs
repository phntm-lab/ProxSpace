//! `clean`: get the disk space back, or throw the environment away.
//!
//! The original had nothing like this — removing a ProxSpace meant deleting the
//! folder in Explorer, which also deleted whatever the user had been working on
//! for the last six months, because the proxmark3 checkouts live inside it.
//!
//! That is the one rule this module is built around: `pm3/` and `builds/` are
//! never touched by anything here. Everything that can be removed is something
//! this binary downloaded and can download again — the msys2 tree, the package
//! cache inside it, and a base archive left behind by an interrupted install.

use std::fs;
use std::path::PathBuf;

use thiserror::Error;

use crate::core::pacman::Cache;
use crate::core::paths::Paths;
use crate::core::state::{State, StateError};
use crate::infra::archive::{self, ExtractError};
use crate::infra::download;
use crate::infra::msys2::procs::{self, ProcsError};
use crate::infra::msys2::{ArchiveSource, procs::Stopped};
use crate::infra::pacman::{Pacman, PacmanError};
use crate::infra::state as state_file;
use crate::ports::command::CommandRunner;
use crate::ui::{Ui, UiError};

/// How much to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Packages pacman downloaded, which it can download again. The default,
    /// because it frees a lot and costs nothing but a slower reinstall.
    Cache,
    /// The whole msys2 tree.
    All,
}

#[derive(Debug, Error)]
pub enum CleanError {
    #[error("`{path}` is not there; there is nothing to clean")]
    TreeMissing { path: PathBuf },
    #[error("nothing was removed")]
    Refused,
    #[error(transparent)]
    Pacman(#[from] PacmanError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Procs(#[from] ProcsError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Ui(#[from] UiError),
}

/// Do whichever of the two the user asked for.
pub fn run(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    scope: Scope,
) -> Result<(), CleanError> {
    match scope {
        Scope::Cache => cache(runner, ui, paths),
        Scope::All => all(ui, paths, state),
    }
}

/// Empty the pacman package cache.
pub fn cache(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Result<(), CleanError> {
    let tree = require_tree(paths)?;

    ui.step("removing downloaded packages from the pacman cache");
    Pacman::new(&tree).clean_cache(runner, ui, Cache::All)?;
    ui.success("the package cache is empty");
    Ok(())
}

/// Remove the msys2 tree, and anything else this binary downloaded.
///
/// What survives is everything the user made: `pm3/`, `builds/`, the log and
/// the state file. The state is walked back to the beginning rather than
/// deleted, so the next run installs from scratch instead of finding a stage
/// recorded for a tree that is no longer there.
pub fn all(ui: &Ui, paths: &Paths, state: &mut State) -> Result<(), CleanError> {
    let tree = require_tree(paths)?;

    ui.info(&format!("this removes `{}`", tree.display()));
    ui.info(&format!(
        "`{}` and `{}` are kept",
        paths.pm3().display(),
        paths.builds().display()
    ));
    if !ui.confirm("Remove the msys2 tree?", false)? {
        return Err(CleanError::Refused);
    }

    // The tree cannot be removed while a shell, a build or a gpg-agent still
    // has a file in it open — on Windows that is a hard error, not a warning.
    let stopped: Stopped = procs::stop_holders(&tree, ui)?;
    if !stopped.killed.is_empty() {
        ui.detail("everything using the tree has been stopped");
    }

    ui.step("removing the msys2 tree");
    archive::remove_tree(&tree)?;
    discard_leftover_archive(ui, paths);

    state.forget_msys2()?;
    state_file::save(state, &paths.state_file())?;

    ui.success("the environment is gone; run ProxSpace again to install it afresh");
    Ok(())
}

/// The msys2 tree, or an error saying it is not there.
fn require_tree(paths: &Paths) -> Result<PathBuf, CleanError> {
    let tree = paths.msys2();
    if tree.is_dir() {
        Ok(tree)
    } else {
        Err(CleanError::TreeMissing { path: tree })
    }
}

/// Remove a base archive an interrupted install left next to the binary.
///
/// Tens of megabytes that nothing will ever look at again once the tree they
/// were meant to become is gone. Failing to delete one is not worth failing the
/// command over.
fn discard_leftover_archive(ui: &Ui, paths: &Paths) {
    let archive = ArchiveSource::msys2().archive_path(paths);
    for path in [download::part_path(&archive), archive] {
        if !path.exists() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => ui.detail(&format!("removed `{}`", path.display())),
            Err(error) => ui.warn(&format!("cannot remove `{}` ({error})", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::state::Stage;
    use crate::ports::command::{Cmd, CommandError, Output};
    use crate::ui::UiOptions;
    use crate::ui::logging::Logger;

    fn ui(assume_yes: bool) -> Ui {
        Ui::new(
            UiOptions {
                quiet: true,
                assume_yes,
                ..UiOptions::default()
            },
            Arc::new(Logger::disabled()),
        )
    }

    /// A runner that must never be called.
    struct NeverRuns;

    impl CommandRunner for NeverRuns {
        fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
            panic!("nothing should be run: {}", cmd.command_line());
        }
    }

    /// An installation with a tree, a checkout the user cares about, and a
    /// finished state.
    fn installed() -> (tempfile::TempDir, Paths, State) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::infra::paths::from_dir(dir.path()).unwrap();

        fs::create_dir_all(paths.msys2().join("usr/bin")).unwrap();
        fs::write(paths.msys2().join("usr/bin/bash.exe"), b"tree").unwrap();
        fs::create_dir_all(paths.pm3().join("proxmark3")).unwrap();
        fs::write(paths.pm3().join("proxmark3/six-months-of-work.c"), b"mine").unwrap();
        fs::create_dir_all(paths.builds()).unwrap();
        fs::write(paths.builds().join("pm3.exe"), b"built").unwrap();

        let state = State {
            stage: Stage::Ready,
            pip_extras_installed: true,
            ..State::default()
        };
        (dir, paths, state)
    }

    /// The rule the whole module exists for.
    #[test]
    fn removing_everything_keeps_what_the_user_made() {
        let (_dir, paths, mut state) = installed();

        all(&ui(true), &paths, &mut state).unwrap();

        assert!(!paths.msys2().exists(), "the tree must be gone");
        assert!(paths.pm3().join("proxmark3/six-months-of-work.c").is_file());
        assert!(paths.builds().join("pm3.exe").is_file());
    }

    #[test]
    fn removing_everything_walks_the_state_back_to_the_beginning() {
        let (_dir, paths, mut state) = installed();

        all(&ui(true), &paths, &mut state).unwrap();

        assert_eq!(state.stage, Stage::NotInstalled);
        assert!(state.msys2.is_none());
        assert!(state.packages.is_none());
        assert!(!state.pip_extras_installed);
        // Written out, not only changed in memory.
        let saved = state_file::load(&paths.state_file()).state;
        assert_eq!(saved.stage, Stage::NotInstalled);
    }

    /// Without an answer there is no deletion — and without a terminal there is
    /// no answer, which is exactly the unattended case that must not guess.
    #[test]
    fn nothing_is_removed_without_a_confirmation() {
        let (_dir, paths, mut state) = installed();

        assert!(all(&ui(false), &paths, &mut state).is_err());

        assert!(paths.msys2().is_dir(), "the tree must still be there");
        assert_eq!(state.stage, Stage::Ready);
    }

    #[test]
    fn a_half_downloaded_archive_goes_with_the_tree() {
        let (_dir, paths, mut state) = installed();
        let archive = ArchiveSource::msys2().archive_path(&paths);
        fs::write(&archive, b"tens of megabytes").unwrap();
        fs::write(download::part_path(&archive), b"and a partial one").unwrap();

        all(&ui(true), &paths, &mut state).unwrap();

        assert!(!archive.exists());
        assert!(!download::part_path(&archive).exists());
    }

    #[test]
    fn cleaning_a_directory_with_no_tree_says_so_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::infra::paths::from_dir(dir.path()).unwrap();
        let mut state = State::default();

        assert!(matches!(
            cache(&NeverRuns, &ui(true), &paths),
            Err(CleanError::TreeMissing { .. })
        ));
        assert!(matches!(
            all(&ui(true), &paths, &mut state),
            Err(CleanError::TreeMissing { .. })
        ));
    }

    #[test]
    fn the_cache_is_emptied_with_pacman_and_the_tree_is_left_alone() {
        #[derive(Default)]
        struct Fake(std::sync::Mutex<Vec<String>>);

        impl CommandRunner for Fake {
            fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
                self.0.lock().unwrap().push(cmd.command_line());
                Ok(Output::new(Some(0), "", "", cmd.describe()))
            }
        }

        let (_dir, paths, _state) = installed();
        fs::write(paths.msys2().join("usr/bin/pacman.exe"), b"pacman").unwrap();
        let fake = Fake::default();

        cache(&fake, &ui(true), &paths).unwrap();

        let calls = fake.0.lock().unwrap().clone();
        assert!(
            calls.iter().any(|call| call.contains("-Scc")),
            "got: {calls:?}"
        );
        assert!(paths.msys2().is_dir(), "`--cache` must not remove the tree");
    }
}
