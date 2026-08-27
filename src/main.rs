//! Command-line entry point: parse, set up output and logging, dispatch.
//!
//! Everything of substance lives in the `proxspace` library next to this file.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use proxspace::clean::{self, Scope};
use proxspace::cli::{Cli, Command, EXIT_NOT_IMPLEMENTED, MirrorsAction};
use proxspace::command::ProcessRunner;
use proxspace::http::UreqClient;
use proxspace::info;
use proxspace::install::{self, Plan};
use proxspace::interrupt::{self, EXIT_INTERRUPTED};
use proxspace::logging::{Level, Logger};
use proxspace::mirrors;
use proxspace::msys2::shell;
use proxspace::paths::Paths;
use proxspace::preflight;
use proxspace::state::State;
use proxspace::ui::{self, Ui, UiOptions};

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
    ExitCode::from(code as u8)
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
    let mut state = loaded.state;
    ui.detail(&format!("install state: {}", state.stage));

    dispatch(&command, &ui, &paths, &mut state)
}

/// How far [`ensure_environment`] got.
enum Ready {
    Yes,
    /// Stopped by Ctrl+C. Not an error: the state file records what did finish,
    /// and the next run carries on from there.
    Interrupted,
}

/// Bring the environment to the point where it can be used.
///
/// Shared by `install` and `shell` because they differ only in what happens
/// afterwards — the automaton that gets there is the same one, and running it
/// before the shell is what removes the two-launch dance of `runme64.bat`.
fn ensure_environment(ui: &Ui, paths: &Paths, state: &mut State, force: bool) -> Result<Ready> {
    let plan = Plan::shipped(paths)?.forced(force);
    match install::ensure_ready(&UreqClient::new(), &ProcessRunner, ui, paths, state, &plan) {
        Ok(()) => Ok(Ready::Yes),
        // Ctrl+C surfaces as whichever step noticed it first; the state file
        // already says how far the install got.
        Err(error) if interrupt::requested() => {
            ui.detail(&format!("stopped: {error}"));
            Ok(Ready::Interrupted)
        }
        Err(error) => Err(error.into()),
    }
}

fn dispatch(command: &Command, ui: &Ui, paths: &Paths, state: &mut State) -> Result<i32> {
    match command {
        // The one command that has to keep working on a broken install, which
        // is why it neither runs preflight nor brings the environment up.
        Command::Info => {
            info::run(&ProcessRunner, ui, paths, state);
            Ok(0)
        }

        Command::Install { force } => match ensure_environment(ui, paths, state, *force)? {
            Ready::Yes => Ok(0),
            Ready::Interrupted => Ok(EXIT_INTERRUPTED),
        },

        // The `runme64.bat` case, and the reason the whole install pipeline is
        // resumable: whatever is left to do is done first, then the user gets
        // the shell they asked for. There is no second run of anything.
        Command::Shell { args } => match ensure_environment(ui, paths, state, false)? {
            Ready::Interrupted => Ok(EXIT_INTERRUPTED),
            Ready::Yes => {
                ui.detail("starting the login shell");
                // Its exit code becomes ours: `shell -- -c "make"` is then
                // usable from a script.
                Ok(shell::run(paths, args)?)
            }
        },

        // The scriptable form of the above. It brings the environment up too:
        // a command that needs the toolchain needs it installed, and choosing
        // otherwise would mean an `exec` that fails differently depending on
        // what the user happened to have run before.
        Command::Exec { command } => match ensure_environment(ui, paths, state, false)? {
            Ready::Interrupted => Ok(EXIT_INTERRUPTED),
            Ready::Yes => Ok(shell::exec(paths, command)?),
        },
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

        other => {
            if interrupt::requested() {
                return Ok(EXIT_INTERRUPTED);
            }
            ui.error(&format!("`{}` is not implemented yet", other.name()));
            Ok(EXIT_NOT_IMPLEMENTED)
        }
    }
}
