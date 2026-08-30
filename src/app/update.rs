//! `update`: bring the environment up to what this build of ProxSpace expects.
//!
//! Two halves, which are independent and can be asked for separately:
//!
//! - **msys2** — the tree itself. Usually `pacman -Syuu` where it stands, which
//!   is also what keeps the packages in it current between msys2 releases. When
//!   the tree is too old for an upgrade to reach the shipped version, the whole
//!   tree is replaced instead, and nothing gets that far without being agreed
//!   to;
//! - **packages** — the ProxSpace package list. A newer ProxSpace ships a
//!   longer list, and what is missing from it is installed. Nothing to do with
//!   msys2 versions, which is why it is a half of its own.
//!
//! The original had no equivalent worth speaking of: `ps-upgrade` replaced the
//! ProxSpace scripts from git and ran `pacman -Syuu`, with no notion of which
//! msys2 the environment was built from and no way to find out what a run would
//! do before it did it.

use thiserror::Error;

use crate::app::install::{self, InstallError, Plan};
use crate::app::release;
use crate::core::paths::Paths;
use crate::core::state::State;
use crate::core::update::{Reinstall, Update, decide_update};
use crate::infra::msys2::shell::BASH;
use crate::infra::state as state_file;
use crate::ports::command::CommandRunner;
use crate::ports::http::HttpClient;
use crate::ui::Ui;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error(transparent)]
    Install(#[from] InstallError),
}

/// What the flags of `update` add up to.
pub struct Options {
    /// `--msys2`: the tree only.
    pub msys2: bool,
    /// `--packages`: the package list only.
    pub packages: bool,
    /// `--check`: say what would happen and do none of it.
    pub check: bool,
    /// `--reinstall-msys2` / `--no-reinstall`.
    pub reinstall: Reinstall,
}

impl Options {
    /// Neither half named means both: `update` on its own updates everything.
    fn wants_msys2(&self) -> bool {
        self.msys2 || !self.packages
    }

    fn wants_packages(&self) -> bool {
        self.packages || !self.msys2
    }
}

/// Whether the run was carried out or only described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// `--check`: the plan was printed and nothing was touched.
    Checked,
}

pub fn run(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
    options: &Options,
) -> Result<Outcome, UpdateError> {
    // Decided before anything runs, and for both halves, so that `--check`
    // prints the same thing a real run is about to do.
    let tree = options
        .wants_msys2()
        .then(|| plan_update(paths, state, plan, options.reinstall));
    let packages_current = install::packages_are_current(state, &plan.list);

    if let Some(Update::Blocked { from, to }) = &tree {
        return Err(InstallError::ReinstallForbidden {
            from: from.clone(),
            to: to.clone(),
        }
        .into());
    }

    if options.check {
        report(ui, options, tree.as_ref(), packages_current);
        release::mention_newer(http, ui);
        return Ok(Outcome::Checked);
    }

    if let Some(tree) = tree {
        update_tree(http, runner, ui, paths, state, plan, tree)?;
    }

    if options.wants_packages() {
        // Run even when the list has not changed: it is what says so, and what
        // puts the list into a tree the half above may have just installed.
        install::update_packages(runner, ui, paths, state, plan)?;
    }

    ui.success("the environment is up to date");

    // Last, and never fatal: the environment has already been updated by this
    // point, and the binary itself is the one thing this cannot update.
    release::mention_newer(http, ui);
    Ok(Outcome::Done)
}

/// Do to the tree whatever the matrix decided.
fn update_tree(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
    tree: Update,
) -> Result<(), UpdateError> {
    if !confirm_update(ui, &tree) {
        return Ok(());
    }

    match tree {
        // Nothing is there yet, so the update is an install.
        Update::Install => install::ensure_ready(http, runner, ui, paths, state, plan)?,
        Update::Reinstall { .. } => install::reinstall_msys2(http, runner, ui, paths, state, plan)?,
        // The rest are the same command; they differ only in what the state
        // file should say afterwards.
        Update::UpToDate { .. } | Update::Newer { .. } | Update::Upgrade { .. } => {
            install::upgrade_msys2(runner, ui, paths, plan)?;
            if let Update::Upgrade { to, .. } = &tree {
                record_version(ui, paths, state, to)?;
            }
        }
        // Refused before this is reached.
        Update::Blocked { .. } => {}
    }
    Ok(())
}

/// Write the version the tree has just been brought up to.
///
/// Only after the upgrade has actually finished: a state file claiming a
/// version the tree never reached would send the next run down the wrong row of
/// the matrix, and the wrong row here is the one that deletes things.
fn record_version(
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    version: &str,
) -> Result<(), UpdateError> {
    let Some(info) = state.msys2.as_mut() else {
        return Ok(());
    };
    info.version = version.to_string();
    state_file::save(state, &paths.state_file()).map_err(InstallError::from)?;
    ui.detail(&format!("the tree is recorded as msys2 {version}"));
    Ok(())
}

/// `--check`: what a real run would do, from the state file and the versions
/// alone.
///
/// Deliberately cheap. Naming the exact packages would mean asking pacman,
/// which means a working tree and a command that can fail — and a check that
/// fails is no use for finding out whether anything is wrong.
fn report(ui: &Ui, options: &Options, tree: Option<&Update>, packages_current: bool) {
    if let Some(tree) = tree {
        ui.info(&tree.summary());
    }
    if options.wants_packages() {
        ui.info(if packages_current {
            "the package set is the one this ProxSpace ships"
        } else {
            "this ProxSpace ships a different package set; \
             the packages missing from this environment would be installed"
        });
    }
    ui.info("nothing was changed (--check)");
}

/// Decide what an update run does, from the state file and what is on disk.
pub fn plan_update(paths: &Paths, state: &State, plan: &Plan, reinstall: Reinstall) -> Update {
    // Both halves have to agree that there is a tree: a state file left behind
    // by a deleted folder describes nothing, and a folder no state file knows
    // about cannot be told apart from a half-finished install.
    let installed = match (&state.msys2, paths.msys2().join(BASH).is_file()) {
        (Some(info), true) => Some(info.version.as_str()),
        _ => None,
    };
    decide_update(
        installed,
        &plan.source.version,
        &plan.min_compatible,
        reinstall,
    )
}

/// Show the plan, and get it agreed to when it destroys the tree.
///
/// Returns whether to go ahead. The plan is always shown before anything
/// happens: an update that turns out to mean "your five gigabytes are about to
/// be deleted" should never be a surprise.
pub fn confirm_update(ui: &Ui, update: &Update) -> bool {
    match update {
        Update::Newer { .. } | Update::Blocked { .. } => ui.warn(&update.summary()),
        _ => ui.info(&update.summary()),
    }

    if update.is_blocked() {
        return false;
    }
    if !update.destroys_the_tree() {
        return true;
    }

    ui.info("`pm3` and `builds` are not touched; the proxmark3 sources and anything built from them stay");
    match ui.confirm("delete the msys2 tree and install it again?", false) {
        Ok(answer) => {
            if !answer {
                ui.info("left as it is; the environment goes on working as before");
            }
            answer
        }
        // No terminal to ask on and no `--yes`. Deleting gigabytes on a guess
        // is the one thing that must not happen here.
        Err(_) => {
            ui.warn(
                "cannot ask whether to reinstall msys2; run the command again with `--yes` \
                 to agree to it in advance",
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(msys2: bool, packages: bool) -> Options {
        Options {
            msys2,
            packages,
            check: false,
            reinstall: Reinstall::WhenNeeded,
        }
    }

    #[test]
    fn naming_neither_half_means_both() {
        let both = options(false, false);
        assert!(both.wants_msys2() && both.wants_packages());
    }

    #[test]
    fn naming_one_half_excludes_the_other() {
        let tree_only = options(true, false);
        assert!(tree_only.wants_msys2() && !tree_only.wants_packages());

        let packages_only = options(false, true);
        assert!(!packages_only.wants_msys2() && packages_only.wants_packages());
    }

    #[test]
    fn naming_both_halves_is_the_same_as_naming_neither() {
        let both = options(true, true);
        assert!(both.wants_msys2() && both.wants_packages());
    }
}
