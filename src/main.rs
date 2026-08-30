//! Command-line entry point: parse, set up output and logging, dispatch.
//!
//! Everything of substance lives in the `proxspace` library next to this file.
//!
//! Exit codes, which are part of what this binary promises to scripts:
//!
//! - `0` — it did what it was asked;
//! - `1` — it failed, and said why on stderr and in the log;
//! - `2` — the command line itself was wrong (clap's own code);
//! - `130` — stopped by Ctrl+C, the shell convention of 128 + SIGINT.
//!
//! `shell`, `exec` and `autobuild` hand back the exit code of the program they
//! ran instead, which is what makes them usable in a script; a build that exits
//! 1 is therefore indistinguishable from ProxSpace failing to start it, and
//! that is the trade the passthrough is worth.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use proxspace::autobuild;
use proxspace::clean::{self, Scope};
use proxspace::cli::{Cli, Command, MirrorsAction};
use proxspace::command::ProcessRunner;
use proxspace::http::UreqClient;
use proxspace::info;
use proxspace::install::{self, Plan, Reinstall};
use proxspace::interrupt::{self, EXIT_INTERRUPTED};
use proxspace::logging::{Level, Logger};
use proxspace::mirrors;
use proxspace::msys2::shell;
use proxspace::paths::Paths;
use proxspace::preflight;
use proxspace::state::{SCHEMA_VERSION, State};
use proxspace::ui::{self, Ui, UiOptions};
use proxspace::update::{self, Options, Outcome};

fn main() -> ExitCode {
    // `Cli::parse` exits by itself on `--help`, `--version` and usage errors,
    // so nothing below runs for those — in particular no log file is created.
    let cli = Cli::parse();

    let mut logger = Arc::new(Logger::disabled());
    let code = match run(cli, &mut logger) {
        Ok(code) => code,
        Err(error) => {
            report(&logger, &error);
            // Double-clicked from Explorer, the console goes with us; without
            // this the message above would never be read.
            ui::hold_window_open();
            1
        }
    };
    ExitCode::from(exit_code(code))
}

/// Narrow an exit code to the byte a process can actually return.
///
/// Only the passthrough codes of `shell`, `exec` and `autobuild` can be
/// anything else, and on Windows they can be wild: a program killed by an
/// access violation exits with `0xC0000005`, which truncated to a byte is `5` —
/// a number that means "it worked, mostly" to whatever reads it. Anything that
/// does not fit becomes a plain failure instead.
fn exit_code(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

/// Print an error and its full cause chain to stderr and to the log.
fn report(logger: &Logger, error: &anyhow::Error) {
    let mut text = format!("{error}");
    for cause in error.chain().skip(1) {
        text.push_str(&format!("\n  caused by: {cause}"));
    }
    logger.write(Level::Error, &text);
    eprintln!("{} {text}", console::style("error:").red().bold());

    let log = logger.path();
    if !log.as_os_str().is_empty() && log.exists() {
        eprintln!("see {} for the full log", log.display());
    }
}

fn run(cli: Cli, logger_out: &mut Arc<Logger>) -> Result<i32> {
    let paths = Paths::discover(cli.global.dir.as_deref())
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
        let checks = preflight::run(&paths).context("environment check failed")?;
        for warning in &checks.warnings {
            ui.warn(warning);
        }
    }

    let loaded = State::load(&paths.state_file());
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
            install::repair(&ProcessRunner, ui, paths, &Plan::shipped(paths)?, *rebase)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_this_binary_promises_are_returned_unchanged() {
        assert_eq!(exit_code(0), 0);
        assert_eq!(exit_code(1), 1);
        assert_eq!(exit_code(2), 2);
        assert_eq!(exit_code(EXIT_INTERRUPTED), 130);
    }

    #[test]
    fn a_code_that_is_not_a_byte_becomes_a_plain_failure() {
        // What a program killed by an access violation exits with. Truncated
        // to a byte it would be 5, and 5 is not what happened.
        assert_eq!(exit_code(0xC000_0005u32 as i32), 1);
        assert_eq!(exit_code(256), 1);
        assert_eq!(exit_code(-1), 1);
    }
}
