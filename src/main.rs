//! Command-line entry point: parse, set up output and logging, dispatch.
//!
//! Everything of substance lives in the `proxspace` library next to this file.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use proxspace::cli::{Cli, Command, EXIT_NOT_IMPLEMENTED};
use proxspace::interrupt::{self, EXIT_INTERRUPTED};
use proxspace::logging::{Level, Logger};
use proxspace::paths::Paths;
use proxspace::preflight;
use proxspace::state::State;
use proxspace::ui::{Ui, UiOptions};

fn main() -> ExitCode {
    // `Cli::parse` exits by itself on `--help`, `--version` and usage errors,
    // so nothing below runs for those — in particular no log file is created.
    let cli = Cli::parse();

    let mut logger = Arc::new(Logger::disabled());
    let code = match run(cli, &mut logger) {
        Ok(code) => code,
        Err(error) => {
            report(&logger, &error);
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
    let state = loaded.state;
    ui.detail(&format!("install state: {}", state.stage));
    if state.was_moved_from(paths.base()) {
        ui.warn(&format!(
            "this environment was installed in `{}` and has been moved here; \
             the packages will need reinstalling",
            state.install_path.as_deref().unwrap_or("?")
        ));
    }

    dispatch(&command, &ui, &paths, &state)
}

fn dispatch(command: &Command, ui: &Ui, paths: &Paths, state: &State) -> Result<i32> {
    match command {
        // `info` is the one command that can already say something useful, and
        // the one that has to keep working on a broken install.
        Command::Info => {
            ui.output(&format!("proxspace  {}", proxspace::VERSION));
            ui.output(&format!("base       {}", paths.base().display()));
            ui.output(&format!("msys2      {}", paths.msys2().display()));
            ui.output(&format!("home       {}", paths.pm3().display()));
            ui.output(&format!("state      {}", state.stage));
            match &state.msys2 {
                Some(info) => ui.output(&format!(
                    "msys2 base {} (extracted {})",
                    info.version, info.extracted_at
                )),
                None => ui.output("msys2 base not installed"),
            }
            ui.output(&format!("log        {}", ui.logger().path().display()));
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
