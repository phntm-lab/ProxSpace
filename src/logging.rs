//! Append-only log file kept next to the binary.
//!
//! Everything the user sees, plus (from the msys2 work onward) the full output
//! of every external command, is mirrored here. The original ProxSpace kept no
//! log at all, which made "it failed somewhere in pacman" unanswerable after
//! the console window closed.
//!
//! Deliberately not `tracing` or `log`: a single file, a single process and no
//! filtering do not justify a logging framework and its backends.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Rotation threshold. Package installs are chatty, so a few runs can produce
/// megabytes; the previous file is kept so that the run before the one that
/// finally failed is still readable.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Step,
    Warn,
    Error,
    Debug,
    /// Output captured from an external process.
    Command,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Level::Info => "INFO",
            Level::Step => "STEP",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Debug => "DEBUG",
            Level::Command => "CMD",
        };
        f.write_str(text)
    }
}

/// A log file, or a no-op sink when the file could not be opened.
///
/// Failing to open the log is never fatal: a read-only folder or a locked file
/// must not stop the user from running a shell. The reason surfaces once, as a
/// warning, via [`Logger::open_warning`].
pub struct Logger {
    sink: Mutex<Option<File>>,
    path: PathBuf,
    open_warning: Option<String>,
}

impl Logger {
    /// Open (rotating first if needed) the log at `path`, keeping the previous
    /// contents at `backup_path`.
    pub fn open(path: &Path, backup_path: &Path) -> Logger {
        let mut open_warning = rotate_if_large(path, backup_path);

        let file = match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => Some(file),
            Err(error) => {
                open_warning = Some(format!(
                    "cannot write the log file `{}` ({error}); continuing without a log",
                    path.display()
                ));
                None
            }
        };

        Logger {
            sink: Mutex::new(file),
            path: path.to_path_buf(),
            open_warning,
        }
    }

    /// A logger that discards everything, for tests and for `--help`-style
    /// invocations that must not create files.
    pub fn disabled() -> Logger {
        Logger {
            sink: Mutex::new(None),
            path: PathBuf::new(),
            open_warning: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open_warning(&self) -> Option<&str> {
        self.open_warning.as_deref()
    }

    /// Write one entry. Multi-line messages are split so that every line in the
    /// file carries a timestamp and level — otherwise a captured build log
    /// makes the surrounding entries unfindable.
    pub fn write(&self, level: Level, message: &str) {
        let Ok(mut guard) = self.sink.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else {
            return;
        };

        let stamp = crate::state::timestamp();
        for line in message.lines() {
            // A write failure here (disk full, file deleted underneath us) is
            // deliberately swallowed: losing the log must not abort the work
            // the log is describing.
            let _ = writeln!(file, "{stamp} [{level}] {line}");
        }
        let _ = file.flush();
    }

    /// Mark the start of a run so that entries from separate runs of the same
    /// day can be told apart.
    pub fn write_session_header(&self, command_line: &str) {
        self.write(
            Level::Info,
            &format!(
                "--- proxspace {} starting: {command_line}",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }
}

/// Move the current log aside once it grows past [`MAX_LOG_BYTES`].
/// Returns a warning if rotation was needed but failed.
fn rotate_if_large(path: &Path, backup_path: &Path) -> Option<String> {
    let size = fs::metadata(path).ok()?.len();
    if size <= MAX_LOG_BYTES {
        return None;
    }
    match fs::rename(path, backup_path) {
        Ok(()) => None,
        Err(error) => Some(format!(
            "cannot rotate the log file `{}` ({error}); it will keep growing",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_carry_a_level_and_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let logger = Logger::open(&path, &dir.path().join("proxspace.log.old"));
        logger.write(Level::Warn, "something to note");

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[WARN] something to note"), "got: {text}");
    }

    #[test]
    fn multi_line_messages_become_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let logger = Logger::open(&path, &dir.path().join("proxspace.log.old"));
        logger.write(Level::Command, "first\nsecond");

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|line| line.contains("[CMD]")));
    }

    #[test]
    fn appends_across_loggers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let backup = dir.path().join("proxspace.log.old");

        Logger::open(&path, &backup).write(Level::Info, "first run");
        Logger::open(&path, &backup).write(Level::Info, "second run");

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("first run") && text.contains("second run"));
    }

    #[test]
    fn oversized_logs_rotate_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let backup = dir.path().join("proxspace.log.old");
        fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();

        let logger = Logger::open(&path, &backup);
        logger.write(Level::Info, "fresh start");

        assert_eq!(fs::metadata(&backup).unwrap().len(), MAX_LOG_BYTES + 1);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("fresh start"));
        assert!(!text.contains('x'));
    }

    #[test]
    fn small_logs_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxspace.log");
        let backup = dir.path().join("proxspace.log.old");
        fs::write(&path, b"previous run\n").unwrap();

        Logger::open(&path, &backup).write(Level::Info, "next run");

        assert!(!backup.exists());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("previous run") && text.contains("next run"));
    }

    #[test]
    fn a_disabled_logger_writes_nothing() {
        let logger = Logger::disabled();
        logger.write(Level::Error, "ignored");
        assert!(logger.open_warning().is_none());
    }
}
