//! Running another program, for real.
//!
//! The only implementation of [`CommandRunner`] that starts a process. What
//! a command is and what came back is described in
//! [`crate::ports::command`]; this spawns it and pumps its output through the
//! [`Ui`] as it arrives, rather than collecting it and printing it at the end.

use std::io::{BufRead, BufReader, Read};
use std::process::{self, Stdio};

use crate::ports::command::{Cmd, CommandError, CommandRunner, Echo, Output};
use crate::ui::Ui;
use crate::ui::interrupt;

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

        Ok(Output::from_status(
            status.code(),
            status.to_string(),
            stdout,
            stderr,
            cmd.describe(),
        ))
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

    use std::sync::Arc;

    use crate::ui::UiOptions;
    use crate::ui::logging::Logger;

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
