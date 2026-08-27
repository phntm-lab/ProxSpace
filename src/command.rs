//! The one way this binary runs an external program.
//!
//! Everything that shells out — `pacman`, `bash`, `rebaseall`, `pip` — goes
//! through the [`CommandRunner`] trait rather than calling
//! [`std::process::Command`] directly, for the same reason downloads go through
//! `HttpClient`: the interesting part of the install is the *order* of those
//! calls and what happens when one of them fails, and no test can afford to run
//! a real `pacman -Syuu`. With the trait in the way, the
//! orchestration is testable against a scripted fake and the real
//! implementation stays small enough to be read rather than tested.
//!
//! What the real implementation has to get right is that these commands are
//! slow and chatty: `pacman -Syuu` can take ten minutes and prints all the way
//! through. Waiting for it with [`std::process::Command::output`] and dumping
//! the result afterwards would leave the user staring at a frozen window, so
//! output is pumped line by line — to the console as it arrives *and* into
//! `proxspace.log` at the same time, which is the whole point of having a log.
//!
//! The command to run is described by [`Cmd`] rather than by
//! [`std::process::Command`]: the latter cannot be inspected once built, so a
//! fake runner could not assert on what it was asked to do.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{self, Stdio};

use thiserror::Error;

use crate::interrupt::{self, Interrupted};
use crate::ui::Ui;

/// How much of the failing output an error message carries. Enough to show the
/// `error:` line pacman ends with, not so much that the message becomes a log.
const ERROR_DETAIL_LINES: usize = 5;

/// Where a command's output goes while it runs.
///
/// Both modes capture it in full and write it to the log; they differ only in
/// what reaches the screen. The log never depends on this, so a failure is
/// explainable after the fact either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Echo {
    /// Shown as it arrives. For the long, interesting commands — installing
    /// packages, upgrading the system — where silence would read as a hang.
    #[default]
    Live,
    /// Shown only with `--verbose`. For commands whose output is data we are
    /// about to parse (`pacman -Q`) or noise nobody asked for.
    Quiet,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("cannot run `{program}`")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot wait for `{program}`")]
    Wait {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{label} failed ({status}){detail}")]
    Failed {
        label: String,
        status: String,
        /// The tail of the output, indented, or empty when there was none.
        detail: String,
    },
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// A command to run: what to run, with what, and how loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Variables added to the inherited environment. msys2 cares a great deal
    /// about `MSYSTEM` and friends, so they are set explicitly rather than
    /// hoped for.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
    pub echo: Echo,
    /// What to call this command when talking to the user. Defaults to the
    /// program's file name; a batch install of sixty packages needs something
    /// shorter than its own command line in an error message.
    label: Option<String>,
}

impl Cmd {
    pub fn new(program: impl Into<PathBuf>) -> Cmd {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            echo: Echo::default(),
            label: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Cmd {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Cmd {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Cmd
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.env.extend(
            vars.into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        self
    }

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Cmd {
        self.cwd = Some(dir.into());
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Cmd {
        self.label = Some(label.into());
        self
    }

    /// Keep the output off the screen unless `--verbose` is on.
    pub fn quiet(mut self) -> Cmd {
        self.echo = Echo::Quiet;
        self
    }

    /// How this command is named in messages.
    pub fn describe(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => format!(
                "`{}`",
                self.program
                    .file_name()
                    .unwrap_or(self.program.as_os_str())
                    .to_string_lossy()
            ),
        }
    }

    /// The whole command line, for the log. Arguments containing spaces are
    /// quoted so that a line copied out of the log can be run as it stands.
    pub fn command_line(&self) -> String {
        let mut text = quote(self.program.as_os_str());
        for arg in &self.args {
            text.push(' ');
            text.push_str(&quote(arg));
        }
        text
    }
}

fn quote(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.is_empty() || text.contains(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text.into_owned()
    }
}

/// What a finished command left behind.
///
/// A non-zero exit is *not* an error here: pacman's exit codes have meanings
/// worth telling apart, and a caller that just wants
/// "must have worked" says so with [`Output::check`].
#[derive(Debug, Clone)]
pub struct Output {
    /// Exit code, or `None` when the process was killed rather than exited.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// How the command was named, carried along so [`Output::check`] can build
    /// an error without being handed the [`Cmd`] again.
    pub label: String,
    /// Textual form of the exit status, for messages: `None` above says only
    /// that there was no code, not what happened instead.
    status: String,
}

impl Output {
    /// Build a result by hand — for a scripted [`CommandRunner`] in tests, and
    /// for the callers that need to reason about output they already have.
    pub fn new(
        code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        label: impl Into<String>,
    ) -> Output {
        Output {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            label: label.into(),
            status: match code {
                Some(code) => format!("exit code: {code}"),
                None => "terminated".to_string(),
            },
        }
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Lines of standard output, without the trailing newline of each.
    pub fn stdout_lines(&self) -> impl Iterator<Item = &str> {
        self.stdout.lines()
    }

    /// Turn a non-zero exit into an error carrying the tail of the output.
    pub fn check(&self) -> Result<(), CommandError> {
        if self.success() {
            return Ok(());
        }
        Err(CommandError::Failed {
            label: self.label.clone(),
            status: self.status.clone(),
            detail: self.detail(),
        })
    }

    /// The last few meaningful lines, preferring stderr — which is where every
    /// tool involved here puts the sentence explaining itself.
    fn detail(&self) -> String {
        let source = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let lines: Vec<&str> = source
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        let tail = &lines[lines.len().saturating_sub(ERROR_DETAIL_LINES)..];
        let mut text = String::new();
        for line in tail {
            text.push_str("\n  ");
            text.push_str(line);
        }
        text
    }
}

pub trait CommandRunner {
    /// Run the command to completion, streaming its output as it arrives.
    ///
    /// Returns `Ok` for any process that ran, whatever it exited with; `Err`
    /// only when it could not be started, could not be waited for, or was
    /// interrupted.
    fn run(&self, ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError>;
}

/// The real runner: spawns the process and pumps its output.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl ProcessRunner {
    pub fn new() -> ProcessRunner {
        ProcessRunner
    }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
        interrupt::check()?;
        ui.detail(&format!("$ {}", cmd.command_line()));

        let mut process = process::Command::new(&cmd.program);
        process
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(key, value)| (key, value)))
            // Nothing here is ever meant to ask a question: every long command
            // is run with `--noconfirm`, and a child inheriting the console
            // input could sit waiting for a keystroke behind a progress bar
            // that gives no hint of it.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &cmd.cwd {
            process.current_dir(cwd);
        }

        let mut child = process.spawn().map_err(|source| CommandError::Spawn {
            program: cmd.program.clone(),
            source,
        })?;

        // `take` cannot fail — both were just asked for as pipes — but an
        // `unwrap` here would be a panic in the one place that must not panic.
        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();

        // Both streams have to be drained at the same time: a pipe holds only a
        // few kilobytes, and a child whose stderr nobody is reading blocks
        // forever once it fills up. Scoped threads so that `ui` can be borrowed
        // rather than shared through an `Arc` nothing else needs.
        let (stdout, stderr) = std::thread::scope(|scope| {
            let errors = scope.spawn(|| pump(child_stderr, ui, cmd.echo));
            let stdout = pump(child_stdout, ui, cmd.echo);
            // A panic in the pump thread would mean a bug in the pump; the
            // output is worth less than the exit status, so take what there is.
            (stdout, errors.join().unwrap_or_default())
        });

        let status = child.wait().map_err(|source| CommandError::Wait {
            program: cmd.program.clone(),
            source,
        })?;

        // Asked after the process is gone, not before: on Windows Ctrl+C
        // reaches the whole console group, so the child normally dies on its
        // own and the pumps end. Reporting the interruption afterwards keeps
        // the two facts — it stopped, and why — in the right order.
        interrupt::check()?;

        Ok(Output {
            code: status.code(),
            stdout,
            stderr,
            label: cmd.describe(),
            status: status.to_string(),
        })
    }
}

/// Read one stream to its end, echoing every line and collecting all of them.
///
/// Bytes rather than [`BufRead::lines`]: a package name in a foreign locale, or
/// a build log with a stray byte in it, would otherwise turn into an I/O error
/// and lose the rest of the output. Whatever cannot be decoded is replaced, and
/// the line is still shown.
fn pump(stream: Option<impl Read>, ui: &Ui, echo: Echo) -> String {
    let Some(stream) = stream else {
        return String::new();
    };
    let mut reader = BufReader::new(stream);
    let mut collected = String::new();
    let mut raw = Vec::new();

    loop {
        raw.clear();
        // A read error here is the pipe breaking because the child is gone —
        // the exit status says what happened, so stop reading and let it.
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&raw);
        // Carriage returns come from progress meters that assumed a terminal;
        // in a file they would overwrite the timestamp of their own line.
        let line = text.trim_end_matches(['\n', '\r']);
        match echo {
            Echo::Live => ui.command_line(line),
            Echo::Quiet => ui.command_detail(line),
        }
        collected.push_str(line);
        collected.push('\n');
    }
    collected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::Logger;
    use crate::ui::{Ui, UiOptions};
    use std::sync::Arc;

    fn silent_ui() -> Ui {
        Ui::new(
            UiOptions {
                quiet: true,
                ..UiOptions::default()
            },
            Arc::new(Logger::disabled()),
        )
    }

    /// Windows' own shell, used as a stand-in for the real commands: it is the
    /// only program guaranteed to be on any machine this binary runs on.
    fn shell(script: &str) -> Cmd {
        Cmd::new("cmd").arg("/C").arg(script).quiet()
    }

    #[test]
    fn a_command_line_quotes_only_what_needs_it() {
        let cmd = Cmd::new("pacman").arg("-S").arg("a b").arg("");
        assert_eq!(cmd.command_line(), "pacman -S \"a b\" \"\"");
    }

    #[test]
    fn the_default_name_is_the_program_not_its_path() {
        let cmd = Cmd::new(r"C:\ProxSpace\msys2\usr\bin\pacman.exe");
        assert_eq!(cmd.describe(), "`pacman.exe`");
        assert_eq!(
            cmd.label("installing packages").describe(),
            "installing packages"
        );
    }

    #[test]
    fn a_failure_carries_the_tail_of_the_output() {
        let output = Output {
            code: Some(1),
            stdout: String::new(),
            stderr: (1..=9).fold(String::new(), |mut text, n| {
                text.push_str(&format!("line {n}\n"));
                text
            }),
            label: "`pacman`".to_string(),
            status: "exit code: 1".to_string(),
        };

        let message = output.check().unwrap_err().to_string();
        assert!(
            message.starts_with("`pacman` failed (exit code: 1)"),
            "got: {message}"
        );
        assert!(message.contains("line 9"));
        assert!(message.contains("line 5"));
        // Only the tail: an error message is not a log.
        assert!(!message.contains("line 4"));
    }

    #[test]
    fn stdout_is_used_when_nothing_went_to_stderr() {
        let output = Output {
            code: Some(2),
            stdout: "only this\n".to_string(),
            stderr: "   \n".to_string(),
            label: "`pacman`".to_string(),
            status: "exit code: 2".to_string(),
        };
        assert!(
            output
                .check()
                .unwrap_err()
                .to_string()
                .contains("only this")
        );
    }

    #[test]
    fn a_success_checks_out_and_keeps_its_output() {
        let output = ProcessRunner
            .run(&silent_ui(), &shell("echo hello"))
            .unwrap();

        assert!(output.success());
        output.check().unwrap();
        assert_eq!(output.stdout_lines().next(), Some("hello"));
    }

    #[test]
    fn a_nonzero_exit_is_reported_but_not_an_error_by_itself() {
        let output = ProcessRunner.run(&silent_ui(), &shell("exit 3")).unwrap();

        assert_eq!(output.code, Some(3));
        assert!(!output.success());
        assert!(output.check().is_err());
    }

    #[test]
    fn both_streams_are_captured_and_kept_apart() {
        let output = ProcessRunner
            .run(&silent_ui(), &shell("echo out& echo err 1>&2"))
            .unwrap();

        assert!(output.stdout.contains("out"), "stdout: {:?}", output.stdout);
        assert!(output.stderr.contains("err"), "stderr: {:?}", output.stderr);
        assert!(!output.stdout.contains("err"));
    }

    #[test]
    fn arguments_and_the_environment_reach_the_process() {
        let cmd = Cmd::new("cmd")
            .arg("/C")
            .arg("echo %PS_TEST_VALUE%")
            .env("PS_TEST_VALUE", "carried through")
            .quiet();

        let output = ProcessRunner.run(&silent_ui(), &cmd).unwrap();
        assert_eq!(output.stdout.trim(), "carried through");
    }

    #[test]
    fn the_working_directory_is_where_it_was_asked_to_be() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = shell("cd").current_dir(dir.path());

        let output = ProcessRunner.run(&silent_ui(), &cmd).unwrap();

        // `cd` with no argument prints the directory; compare canonically,
        // since a temp path can arrive with a short name or a `\\?\` prefix.
        let reported = std::fs::canonicalize(output.stdout.trim()).unwrap();
        assert_eq!(reported, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        // The reason both streams are pumped on their own thread: a child that
        // fills the pipe while nobody reads it never exits. 4000 lines is well
        // past the buffer on any Windows.
        let script = "for /L %i in (1,1,4000) do @echo 0123456789012345678901234567890123456789";
        let output = ProcessRunner.run(&silent_ui(), &shell(script)).unwrap();

        assert!(output.success());
        assert_eq!(output.stdout_lines().count(), 4000);
    }

    #[test]
    fn a_missing_program_says_so_instead_of_hanging() {
        let cmd = Cmd::new("proxspace-no-such-program-exists").quiet();
        let error = ProcessRunner.run(&silent_ui(), &cmd).unwrap_err();

        assert!(matches!(error, CommandError::Spawn { .. }));
        assert!(error.to_string().contains("cannot run"));
    }

    #[test]
    fn output_reaches_the_log_even_when_the_screen_is_quiet() {
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

        ProcessRunner
            .run(&ui, &Cmd::new("cmd").arg("/C").arg("echo recorded"))
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[CMD] recorded"), "got: {text}");
    }
}
