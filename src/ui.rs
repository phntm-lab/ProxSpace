//! The single place that talks to the user.
//!
//! Every message goes through here so that `--quiet`, `--verbose`, `--no-color`
//! and the log file cannot drift apart: a `println!` sprinkled somewhere else
//! would ignore all four.
//!
//! Stream split: progress and results go to stdout, warnings and errors to
//! stderr. `--quiet` silences progress but never warnings, errors or the actual
//! output of a command such as `info` — suppressing those would turn a quiet
//! run into a silent failure.

use std::io::Write;
use std::sync::Arc;

use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use thiserror::Error;

use crate::logging::{Level, Logger};

#[derive(Debug, Error)]
pub enum UiError {
    #[error("{prompt} — no answer possible: stdin is not a terminal; pass --yes to confirm")]
    NotInteractive { prompt: String },
    #[error("cannot read the answer")]
    Prompt(#[source] std::io::Error),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UiOptions {
    pub quiet: bool,
    pub verbose: bool,
    pub assume_yes: bool,
    pub no_color: bool,
}

pub struct Ui {
    options: UiOptions,
    logger: Arc<Logger>,
    interactive: bool,
}

impl Ui {
    pub fn new(options: UiOptions, logger: Arc<Logger>) -> Ui {
        if options.no_color {
            console::set_colors_enabled(false);
            console::set_colors_enabled_stderr(false);
        }
        Ui {
            options,
            logger,
            interactive: console::user_attended_stderr(),
        }
    }

    pub fn logger(&self) -> &Arc<Logger> {
        &self.logger
    }

    pub fn is_quiet(&self) -> bool {
        self.options.quiet
    }

    pub fn is_verbose(&self) -> bool {
        self.options.verbose
    }

    pub fn assumes_yes(&self) -> bool {
        self.options.assume_yes
    }

    /// A major action about to start.
    pub fn step(&self, message: &str) {
        self.logger.write(Level::Step, message);
        if !self.options.quiet {
            println!("{} {message}", style("==>").cyan().bold());
        }
    }

    /// Progress inside the current step.
    pub fn info(&self, message: &str) {
        self.logger.write(Level::Info, message);
        if !self.options.quiet {
            println!("    {message}");
        }
    }

    /// Detail that is always logged but only shown with `--verbose`.
    pub fn detail(&self, message: &str) {
        self.logger.write(Level::Debug, message);
        if self.options.verbose && !self.options.quiet {
            println!("    {}", style(message).dim());
        }
    }

    pub fn success(&self, message: &str) {
        self.logger.write(Level::Info, message);
        if !self.options.quiet {
            println!("{} {message}", style("ok").green().bold());
        }
    }

    pub fn warn(&self, message: &str) {
        self.logger.write(Level::Warn, message);
        eprintln!("{} {message}", style("warning:").yellow().bold());
    }

    pub fn error(&self, message: &str) {
        self.logger.write(Level::Error, message);
        eprintln!("{} {message}", style("error:").red().bold());
    }

    /// Payload a command was asked to produce — printed verbatim, even under
    /// `--quiet`, because it is the reason the command was run.
    pub fn output(&self, text: &str) {
        self.logger.write(Level::Info, text);
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{text}");
    }

    /// Ask a yes/no question.
    ///
    /// `--yes` answers every question affirmatively, which is what makes
    /// unattended runs possible. Without it and without a terminal the answer
    /// is an error rather than a guess: the questions guarded by this are
    /// destructive ones, such as deleting the msys2 tree.
    pub fn confirm(&self, prompt: &str, default: bool) -> Result<bool, UiError> {
        if self.options.assume_yes {
            self.logger
                .write(Level::Info, &format!("{prompt} -> yes (--yes)"));
            return Ok(true);
        }
        if !self.interactive {
            self.logger
                .write(Level::Error, &format!("{prompt} -> no terminal to ask on"));
            return Err(UiError::NotInteractive {
                prompt: prompt.to_string(),
            });
        }

        let answer = dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(default)
            .interact()
            // `dialoguer::Error` is non-exhaustive; flatten it rather than
            // matching variants that may change between releases.
            .map_err(|error| UiError::Prompt(std::io::Error::other(error.to_string())))?;
        self.logger.write(
            Level::Info,
            &format!("{prompt} -> {}", if answer { "yes" } else { "no" }),
        );
        Ok(answer)
    }

    /// Progress bar over a known number of units (bytes, files, packages).
    pub fn progress(&self, total: u64, message: &str) -> ProgressBar {
        if self.options.quiet {
            return ProgressBar::hidden();
        }
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::with_template("    {msg} [{bar:32}] {pos}/{len} ({eta})")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        bar.set_message(message.to_string());
        bar
    }

    /// Progress bar over a transfer measured in bytes.
    ///
    /// Separate from [`Ui::progress`] because a raw `12345678/98765432` is
    /// unreadable for a download: this one formats sizes and shows the rate,
    /// which is what tells the user whether the transfer is alive. A server
    /// that does not announce a size gets a spinner instead of a bar with a
    /// made-up total.
    pub fn progress_bytes(&self, total: Option<u64>, message: &str) -> ProgressBar {
        if self.options.quiet {
            return ProgressBar::hidden();
        }
        let (bar, template) = match total {
            Some(total) => (
                ProgressBar::new(total),
                "    {msg} [{bar:32}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            ),
            None => (
                ProgressBar::new_spinner(),
                "    {spinner} {msg} {bytes} ({bytes_per_sec})",
            ),
        };
        bar.set_style(
            ProgressStyle::with_template(template)
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        bar.set_message(message.to_string());
        bar
    }

    /// Progress over a number of things — files unpacked, packages installed.
    ///
    /// The total is optional because some of that work cannot be counted in
    /// advance: the entries in a compressed archive are only known once it has
    /// been read, and reading it twice to draw a nicer bar is not a trade worth
    /// making. Without a total the user gets a running count instead.
    pub fn progress_items(&self, total: Option<u64>, message: &str) -> ProgressBar {
        match total {
            Some(total) => self.progress(total, message),
            None => {
                if self.options.quiet {
                    return ProgressBar::hidden();
                }
                let bar = ProgressBar::new_spinner();
                bar.set_style(
                    ProgressStyle::with_template("    {spinner} {msg} {pos}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                bar.set_message(message.to_string());
                bar.enable_steady_tick(std::time::Duration::from_millis(120));
                bar
            }
        }
    }

    /// Spinner for work whose size is not known in advance.
    pub fn spinner(&self, message: &str) -> ProgressBar {
        if self.options.quiet {
            return ProgressBar::hidden();
        }
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("    {spinner} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(120));
        bar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(options: UiOptions) -> Ui {
        Ui::new(options, Arc::new(Logger::disabled()))
    }

    #[test]
    fn yes_mode_answers_without_a_terminal() {
        let ui = ui(UiOptions {
            assume_yes: true,
            ..UiOptions::default()
        });
        assert!(ui.confirm("delete everything?", false).unwrap());
    }

    #[test]
    fn without_a_terminal_and_without_yes_a_question_is_an_error() {
        let ui = Ui {
            options: UiOptions::default(),
            logger: Arc::new(Logger::disabled()),
            interactive: false,
        };
        assert!(matches!(
            ui.confirm("delete everything?", false),
            Err(UiError::NotInteractive { .. })
        ));
    }

    #[test]
    fn quiet_mode_hides_progress_bars() {
        let ui = ui(UiOptions {
            quiet: true,
            ..UiOptions::default()
        });
        assert!(ui.progress(10, "downloading").is_hidden());
        assert!(ui.spinner("working").is_hidden());
    }

    #[test]
    fn messages_reach_the_log_regardless_of_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let logger = Arc::new(Logger::open(&path, &dir.path().join("proxspace.log.old")));
        let ui = Ui::new(
            UiOptions {
                quiet: true,
                ..UiOptions::default()
            },
            Arc::clone(&logger),
        );

        ui.step("downloading msys2");
        ui.detail("resolved mirror");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("downloading msys2"));
        // Verbose-only output is still recorded, which is the point of the log.
        assert!(text.contains("resolved mirror"));
    }
}
