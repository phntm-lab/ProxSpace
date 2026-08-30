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

use clap::Parser;

use proxspace::cli::args::Cli;
use proxspace::cli::dispatch;
use proxspace::ui;
use proxspace::ui::logging::{Level, Logger};

fn main() -> ExitCode {
    // `Cli::parse` exits by itself on `--help`, `--version` and usage errors,
    // so nothing below runs for those — in particular no log file is created.
    let cli = Cli::parse();

    let mut logger = Arc::new(Logger::disabled());
    let code = match dispatch::run(cli, &mut logger) {
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

#[cfg(test)]
mod tests {
    use super::*;

    use proxspace::ui::interrupt::EXIT_INTERRUPTED;

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
