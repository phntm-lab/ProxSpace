//! Turning a parsed command line into a run.
//!
//! Everything a command needs is built here — the paths, the log, the output,
//! the state file — and every command then goes through the one `match` in
//! `dispatch`, so that what is set up before a command runs cannot differ
//! between commands.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::app::autobuild;
use crate::app::clean::{self, Scope};
use crate::app::info;
use crate::app::install;
use crate::app::mirrors;
use crate::app::repair;
use crate::app::update::{self, Options, Outcome};
use crate::cli::args::{Cli, Command, MirrorsAction};
use crate::core::paths::Paths;
use crate::core::plan::Plan;
use crate::core::state::{SCHEMA_VERSION, State};
use crate::core::update::Reinstall;
use crate::infra::http::UreqClient;
use crate::infra::msys2::shell;
use crate::infra::process::ProcessRunner;
use crate::infra::state as state_file;
use crate::ui::interrupt::{self, EXIT_INTERRUPTED};
use crate::ui::logging::Logger;
use crate::ui::{Ui, UiOptions};

pub fn run(cli: Cli, logger_out: &mut Arc<Logger>) -> Result<i32> {
    let paths = crate::infra::paths::discover(cli.global.dir.as_deref())
        .context("cannot work out where ProxSpace lives")?;

    let logger = Arc::new(Logger::open(&paths.log_file(), &paths.log_backup_file()));
    *logger_out = Arc::clone(&logger);

    let ui = Ui::new(
        UiOptions {
            quiet: cli.global.quiet,
            verbose: cli.global.verbose,
            assume_yes: cli.global.yes,
            no_color: cli.global.no_color,
        },
        Arc::clone(&logger),
    );

    // No subcommand means the `runme64.bat` case: give the user a shell.
    let command = cli.command.unwrap_or(Command::Shell { args: Vec::new() });

    logger.write_session_header(&format!("{} in {}", command.name(), paths.base().display()));
    if let Some(warning) = logger.open_warning() {
        ui.warn(warning);
    }

    if let Err(error) = interrupt::install(Arc::clone(&logger)) {
        ui.warn(&format!(
            "cannot install the Ctrl+C handler ({error}); interrupting will kill the process outright"
        ));
    }

    ui.detail(&format!("base directory: {}", paths.base().display()));

    if command.needs_preflight() {
        let checks = crate::infra::preflight::run(&paths).context("environment check failed")?;
        for warning in &checks.warnings {
            ui.warn(warning);
        }
    }

    let loaded = state_file::load(&paths.state_file());
    if let Some(warning) = &loaded.warning {
        ui.warn(warning);
    }
    if let Some(from) = loaded.migrated_from {
        ui.info(&format!(
            "state file brought forward from format {from} to {SCHEMA_VERSION}"
        ));
    }
    let mut state = loaded.state;
    ui.detail(&format!("install state: {}", state.stage));

    match dispatch(&command, &ui, &paths, &mut state) {
        Ok(code) => Ok(code),
        // Ctrl+C reaches whichever step was running as an ordinary error — a
        // download that stopped, a pacman that was killed with the console it
        // shared with us. Turning that into the interrupted code here, once,
        // is what keeps every command agreeing on what a stopped run is;
        // whatever did finish is already in the state file, and the next run
        // carries on from there.
        Err(error) if interrupt::requested() => {
            ui.detail(&format!("stopped: {error}"));
            Ok(EXIT_INTERRUPTED)
        }
        Err(error) => Err(error),
    }
}

/// Bring the environment to the point where it can be used.
///
/// Shared by `install` and `shell` because they differ only in what happens
/// afterwards — the automaton that gets there is the same one, and running it
/// before the shell is what removes the two-launch dance of `runme64.bat`.
///
/// A run stopped by Ctrl+C comes back as an error and stops the caller with
/// `?`, which is what keeps a shell from being started on top of a half-built
/// environment; [`run`] turns it into the interrupted exit code.
fn ensure_environment(ui: &Ui, paths: &Paths, state: &mut State, force: bool) -> Result<()> {
    let plan = Plan::shipped(paths)?.forced(force);
    install::ensure_ready(&UreqClient::new(), &ProcessRunner, ui, paths, state, &plan)?;
    Ok(())
}

fn dispatch(command: &Command, ui: &Ui, paths: &Paths, state: &mut State) -> Result<i32> {
    match command {
        // The one command that has to keep working on a broken install, which
        // is why it neither runs preflight nor brings the environment up.
        Command::Info => {
            info::run(&ProcessRunner, ui, paths, state);
            Ok(0)
        }

        Command::Install { force } => {
            ensure_environment(ui, paths, state, *force)?;
            Ok(0)
        }

        // The `runme64.bat` case, and the reason the whole install pipeline is
        // resumable: whatever is left to do is done first, then the user gets
        // the shell they asked for. There is no second run of anything.
        Command::Shell { args } => {
            ensure_environment(ui, paths, state, false)?;
            ui.detail("starting the login shell");
            // Its exit code becomes ours: `shell -- -c "make"` is then usable
            // from a script.
            Ok(shell::run(paths, args)?)
        }

        // The scriptable form of the above. It brings the environment up too:
        // a command that needs the toolchain needs it installed, and choosing
        // otherwise would mean an `exec` that fails differently depending on
        // what the user happened to have run before.
        Command::Exec { command } => {
            ensure_environment(ui, paths, state, false)?;
            Ok(shell::exec(paths, command)?)
        }
        // Two halves that are asked for together by default: the msys2 tree
        // itself, and the package list this build ships. `--check` prints what
        // each of them would do and touches nothing.
        Command::Update {
            msys2,
            packages,
            check,
            reinstall_msys2,
            no_reinstall,
        } => {
            let options = Options {
                msys2: *msys2,
                packages: *packages,
                check: *check,
                reinstall: Reinstall::from_flags(*reinstall_msys2, *no_reinstall),
            };
            match update::run(
                &UreqClient::new(),
                &ProcessRunner,
                ui,
                paths,
                state,
                &Plan::shipped(paths)?,
                &options,
            )? {
                Outcome::Done | Outcome::Checked => Ok(0),
            }
        }

        // Not part of the install automaton: the tree is already there and
        // wrong, so the pipeline that decides what is missing is exactly the
        // wrong tool. Everything installed goes back over itself instead.
        Command::Repair { rebase } => {
            repair::repair(&ProcessRunner, ui, paths, &Plan::shipped(paths)?, *rebase)?;
            Ok(0)
        }

        // Neither half needs the environment brought up: a tree whose mirrors
        // are wrong is one that cannot finish an install in the first place.
        Command::Mirrors { action } => {
            match action {
                MirrorsAction::Rank => mirrors::rank(&ProcessRunner, ui, paths)?,
                MirrorsAction::Restore => mirrors::restore(ui, paths)?,
            }
            Ok(0)
        }

        // `--cache` is the default: it is the one that frees gigabytes without
        // costing anything but a slower reinstall.
        Command::Clean { all, .. } => {
            let scope = if *all { Scope::All } else { Scope::Cache };
            clean::run(&ProcessRunner, ui, paths, state, scope)?;
            Ok(0)
        }

        // Like `shell`: the environment has to be there first, and the build
        // script gets the console, so its exit code becomes ours.
        Command::Autobuild => {
            ensure_environment(ui, paths, state, false)?;
            Ok(autobuild::run(&ProcessRunner, ui, paths)?)
        }
    }
}
