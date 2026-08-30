//! Driving pacman, and the one part of `pacman.conf` ProxSpace owns.
//!
//! This is what the original's `ps-setup` did, with three changes that matter:
//!
//! - packages are installed in **one** transaction instead of a loop calling
//!   `pacman -S` once per name. The original resolved the dependency graph
//!   sixty times and asked the mirrors for it sixty times over;
//! - what is already installed is read once with `pacman -Q` instead of being
//!   probed with `pacman -Q <name>` per package;
//! - a failure is classified rather than passed through. `pacman` exits 1 for
//!   everything from "no network" to "your keyring is broken", and the sentence
//!   that tells the two apart is somewhere in a hundred lines of output the
//!   user has already scrolled past.
//!
//! The pinned `arm-none-eabi-binutils` is why this module also edits
//! `pacman.conf`. The repository build of it breaks the proxmark3 firmware
//! build, so it is installed from a fixed URL and then held there with
//! `IgnorePkg`. The original never touched `pacman.conf`; the block written
//! here is fenced with markers so that it can be rewritten or removed without
//! disturbing anything else in the file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub mod conf;

use crate::core::pacman::{
    Cache, Failure, Installed, MANAGED_BEGIN, MANAGED_END, Mode, count, significant_lines,
};
use crate::infra::msys2;
use crate::infra::msys2::procs;
use crate::ports::command::{Cmd, CommandError, CommandRunner, Output};
use crate::ui::Ui;

/// Where pacman lives inside the tree.
pub const PACMAN_EXE: &str = "usr/bin/pacman.exe";
/// The configuration file whose `IgnorePkg` block is ours.
pub const CONF_PATH: &str = "etc/pacman.conf";
/// Downloaded repository databases.
pub const SYNC_DB_DIR: &str = "var/lib/pacman/sync";
/// Left behind by a pacman that was killed; blocks every later run.
pub const DB_LOCK: &str = "var/lib/pacman/db.lck";

/// Ignoring conflicting files is what the original did (`--overwrite='*'`), and
/// it is still needed: the msys2 repositories ship packages whose files overlap,
/// and without this an install stops halfway with a file-exists error.
///
/// No quotes around the `*`: the original's were for the shell, and there is no
/// shell here — quoting it would make pacman look for a package owning a file
/// literally called `'*'`.
const OVERWRITE_ALL: &str = "--overwrite=*";

#[derive(Debug, Error)]
pub enum PacmanError {
    #[error("`{path}` is not there; msys2 is unpacked but not usable")]
    Missing { path: PathBuf },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("cannot {action} `{path}`")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} failed{}{detail}", .kind.advice())]
    Failed {
        operation: String,
        kind: Failure,
        /// The lines of pacman's own output that say what went wrong.
        detail: String,
    },
    #[error(
        "`{path}` has no `[options]` section, so there is nowhere to keep the \
         package pin; the file is not the one msys2 ships"
    )]
    NoOptionsSection { path: PathBuf },
    #[error(
        "`{path}` has a `{MANAGED_BEGIN}` line with no matching `{MANAGED_END}`; \
         the block was edited by hand and cannot be updated safely — \
         remove what is left of it and run this again"
    )]
    BrokenBlock { path: PathBuf },
}

/// Both of a finished command's streams as one text, which is what the pacman
/// readers in [`crate::core::pacman`] take: pacman splits its complaints
/// between stdout and stderr with no rule worth learning.
fn both_streams(output: &Output) -> String {
    format!(
        "{}
{}",
        output.stdout, output.stderr
    )
}

/// pacman, as reachable inside one msys2 tree.
pub struct Pacman {
    tree: PathBuf,
    exe: PathBuf,
    conf: PathBuf,
}

impl Pacman {
    pub fn new(tree: &Path) -> Pacman {
        Pacman {
            tree: tree.to_path_buf(),
            exe: tree.join(PACMAN_EXE),
            conf: tree.join(CONF_PATH),
        }
    }

    pub fn conf_path(&self) -> &Path {
        &self.conf
    }

    /// Fail early and clearly rather than letting the runner report a missing
    /// file: at this point in the install a missing pacman means the tree is
    /// incomplete, not that the user typed something wrong.
    fn require(&self) -> Result<(), PacmanError> {
        if self.exe.is_file() {
            Ok(())
        } else {
            Err(PacmanError::Missing {
                path: self.exe.clone(),
            })
        }
    }

    /// A pacman invocation, with the environment msys2 programs need
    /// ([`msys2::tool_env`]) and no chance of a question.
    fn cmd(&self, label: &str) -> Cmd {
        Cmd::new(&self.exe)
            .envs(msys2::tool_env(&self.tree))
            .arg("--noconfirm")
            .label(label.to_string())
    }

    /// Run one pacman command and turn a non-zero exit into a classified error.
    fn run(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        cmd: Cmd,
        operation: &str,
    ) -> Result<Output, PacmanError> {
        self.require()?;
        self.clear_stale_lock(ui);

        let output = runner.run(ui, &cmd)?;
        if output.success() {
            return Ok(output);
        }
        Err(PacmanError::Failed {
            operation: operation.to_string(),
            kind: Failure::classify(&both_streams(&output)),
            detail: significant_lines(&both_streams(&output)),
        })
    }

    /// Everything installed right now.
    pub fn query_installed(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
    ) -> Result<Installed, PacmanError> {
        let output = self.run(
            runner,
            ui,
            self.cmd("`pacman -Q`").arg("-Q").quiet(),
            "listing the installed packages",
        )?;
        Ok(Installed::parse(&output.stdout))
    }

    /// Install packages from the repositories, in one transaction.
    pub fn install(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        names: &[&str],
        mode: Mode,
    ) -> Result<(), PacmanError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut cmd = self
            .cmd(&format!("installing {}", count(names.len(), "package")))
            .arg("-S")
            .arg(OVERWRITE_ALL);
        if mode == Mode::Needed {
            cmd = cmd.arg("--needed");
        }
        self.run(
            runner,
            ui,
            cmd.args(names.iter().copied()),
            "installing packages",
        )?;
        Ok(())
    }

    /// Install one package from a URL (`pacman -U`), which is how a pinned
    /// version gets in.
    pub fn install_url(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        url: &str,
    ) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("installing a pinned package")
                .arg("-U")
                .arg(OVERWRITE_ALL)
                .arg(url),
            "installing the pinned package",
        )?;
        Ok(())
    }

    /// `pacman -Syuu`: bring the whole installation, msys2 runtime included, to
    /// what the repositories hold.
    ///
    /// Two `u`s on purpose, as in the original: the second allows going
    /// *backwards*, which is what makes a tree recover after a repository
    /// rolled a package back.
    pub fn system_upgrade(&self, runner: &dyn CommandRunner, ui: &Ui) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("updating msys2").arg("-Syuu").arg(OVERWRITE_ALL),
            "updating msys2",
        )?;
        Ok(())
    }

    /// Throw away the downloaded repository databases.
    ///
    /// Done before the first `-Syuu` because the databases in the base archive
    /// are as old as the archive and were signed by keys the freshly-created
    /// keyring may not have yet; the symptoms are signature errors and
    /// "database file for ... does not exist", both of which look like
    /// something much worse than "fetch them again".
    ///
    /// A filesystem operation rather than `pacman -Syy`: the point is to be
    /// able to do it when pacman itself is what is failing.
    pub fn reset_sync_db(&self, ui: &Ui) -> Result<(), PacmanError> {
        let dir = self.tree.join(SYNC_DB_DIR);
        if !dir.is_dir() {
            return Ok(());
        }
        let entries = fs::read_dir(&dir).map_err(|source| PacmanError::Io {
            action: "read",
            path: dir.clone(),
            source,
        })?;

        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path).map_err(|source| PacmanError::Io {
                    action: "delete",
                    path: path.clone(),
                    source,
                })?;
                removed += 1;
            }
        }
        if removed > 0 {
            ui.detail(&format!(
                "removed {} from `{}`",
                count(removed, "stale database file"),
                dir.display()
            ));
        }
        Ok(())
    }

    /// Drop downloaded packages, which are gigabytes on disk.
    pub fn clean_cache(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        scope: Cache,
    ) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("cleaning the package cache").arg(scope.flag()),
            "cleaning the package cache",
        )?;
        Ok(())
    }

    /// Delete a lock left by a pacman that was killed.
    ///
    /// Only when nothing from the tree is running: a lock with a live pacman
    /// behind it is doing its job, and removing it would let two transactions
    /// write the same database. Failing to remove it is not fatal — the pacman
    /// that follows will say so itself, and by then the classification has an
    /// explanation ready.
    fn clear_stale_lock(&self, ui: &Ui) {
        let lock = self.tree.join(DB_LOCK);
        if !lock.exists() {
            return;
        }
        let holders = procs::find_holders(&self.tree);
        if !holders.is_empty() {
            ui.detail(&format!(
                "`{}` is held by {}",
                lock.display(),
                holders
                    .iter()
                    .map(procs::Holder::describe)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        match fs::remove_file(&lock) {
            Ok(()) => ui.detail(&format!(
                "removed the stale lock `{}` left by an interrupted pacman",
                lock.display()
            )),
            Err(error) => ui.detail(&format!("cannot remove `{}` ({error})", lock.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str, stderr: &str) -> Output {
        Output::new(Some(1), stdout, stderr, "`pacman`")
    }

    #[test]
    fn an_error_says_what_failed_what_to_do_and_what_pacman_printed() {
        let error = PacmanError::Failed {
            operation: "installing packages".to_string(),
            kind: Failure::Network,
            detail: significant_lines(&both_streams(&output(
                "resolving dependencies...\nlooking for conflicting packages...",
                "error: failed retrieving file 'ucrt64.db'\nwarning: too slow",
            ))),
        };

        let message = error.to_string();
        assert!(
            message.starts_with("installing packages failed: the mirrors could not be reached")
        );
        assert!(message.contains("error: failed retrieving file 'ucrt64.db'"));
        assert!(message.contains("warning: too slow"));
        // The chatter before the failure is not part of the message.
        assert!(!message.contains("resolving dependencies"));
    }

    #[test]
    fn an_unknown_failure_says_only_what_failed() {
        let error = PacmanError::Failed {
            operation: "cleaning the package cache".to_string(),
            kind: Failure::Unknown,
            detail: String::new(),
        };
        assert_eq!(error.to_string(), "cleaning the package cache failed");
    }
}
