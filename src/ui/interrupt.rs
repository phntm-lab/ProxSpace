//! Ctrl+C handling.
//!
//! Long operations (downloading a hundred megabytes, unpacking it, running
//! `pacman -Syuu`) must be interruptible without leaving a half-written archive
//! or a state file that claims work which never finished. The handler therefore
//! only raises a flag; the operation in progress notices it at its next
//! checkpoint and unwinds cleanly.
//!
//! A second Ctrl+C means the user is done waiting for a clean stop and gets an
//! immediate exit — and takes the running child with it, because nothing else
//! would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use thiserror::Error;

use crate::ui;
use crate::ui::logging::{Level, Logger};

/// Exit code for a run stopped by Ctrl+C — the shell convention of 128 + SIGINT.
pub const EXIT_INTERRUPTED: i32 = 130;

/// No child is running. A real process never has this id.
const NO_CHILD: u32 = 0;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static CHILD: AtomicU32 = AtomicU32::new(NO_CHILD);

#[derive(Debug, Error)]
#[error("interrupted by Ctrl+C")]
pub struct Interrupted;

/// Install the handler. Failure is not fatal: without it Ctrl+C keeps its
/// default behaviour of killing the process, which is worse but still works.
///
/// `stop_child` is handed the process id of whatever [`watch_child`] last
/// registered, and is expected to end it along with anything it started. It is
/// a parameter rather than a call because stopping a process is not something
/// the screen knows how to do.
pub fn install(
    logger: Arc<Logger>,
    stop_child: impl Fn(u32) + Send + 'static,
) -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(move || {
        // On Windows every process attached to the console is signalled, so a
        // Ctrl+C aimed at a child arrives here as well. While one owns the
        // console the keypress is not ours to act on.
        if PAUSED.load(Ordering::SeqCst) {
            return;
        }
        if REQUESTED.swap(true, Ordering::SeqCst) {
            let message = "second Ctrl+C, exiting immediately";
            logger.write(Level::Warn, message);
            // The child does not go when the console does: a `pacman` signalled
            // mid-transaction carries on downloading for minutes, invisible and
            // still holding its database lock, and every run started meanwhile
            // fails on that lock. Exiting without it is the one thing this path
            // must not do.
            let child = CHILD.swap(NO_CHILD, Ordering::SeqCst);
            if child != NO_CHILD {
                stop_child(child);
            }
            ui::over_progress(|| eprintln!("{message}"));
            std::process::exit(EXIT_INTERRUPTED);
        }
        logger.write(Level::Warn, "Ctrl+C received, stopping at the next step");
        ui::over_progress(|| eprintln!("stopping... press Ctrl+C again to quit immediately"));
    })
}

/// The process this run started, for as long as it is running.
#[derive(Debug)]
pub struct Watched(());

impl Drop for Watched {
    fn drop(&mut self) {
        CHILD.store(NO_CHILD, Ordering::SeqCst);
    }
}

/// Tell an immediate exit what to take with it.
///
/// Only one command runs at a time, so one slot is enough; the guard clears it
/// again so that a Ctrl+C arriving between two commands kills nothing, and so
/// that a pid the operating system has since handed to somebody else is never
/// the one reached for.
///
/// The slot is process-global, like the flags above, which is why it has no
/// test of its own: every other test that runs a real command writes to it.
pub fn watch_child(pid: u32) -> Watched {
    CHILD.store(pid, Ordering::SeqCst);
    Watched(())
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Ctrl+C is being ignored for as long as this lives.
#[derive(Debug)]
pub struct Paused(());

impl Drop for Paused {
    fn drop(&mut self) {
        PAUSED.store(false, Ordering::SeqCst);
    }
}

/// Hand Ctrl+C over to an interactive child process.
///
/// The shell reads the keypress as its own — it is how a user stops a build
/// they just started — and a second one is not a demand that ProxSpace quit.
/// Without this the handler would print over the session and arm the
/// immediate-exit path, taking the shell down with it on the next press.
pub fn pause() -> Paused {
    PAUSED.store(true, Ordering::SeqCst);
    Paused(())
}

/// Checkpoint for long operations: call between units of work.
pub fn check() -> Result<(), Interrupted> {
    if requested() {
        Err(Interrupted)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_requested_by_default() {
        // The flag is process-global, so this only holds because no test in
        // this binary raises it.
        assert!(!requested());
        assert!(check().is_ok());
    }

    #[test]
    fn pausing_lasts_exactly_as_long_as_the_guard() {
        assert!(!PAUSED.load(Ordering::SeqCst));
        {
            let _paused = pause();
            assert!(PAUSED.load(Ordering::SeqCst));
        }
        assert!(!PAUSED.load(Ordering::SeqCst));
    }
}
