//! Ctrl+C handling.
//!
//! Long operations (downloading a hundred megabytes, unpacking it, running
//! `pacman -Syuu`) must be interruptible without leaving a half-written archive
//! or a state file that claims work which never finished. The handler therefore
//! only raises a flag; the operation in progress notices it at its next
//! checkpoint and unwinds cleanly.
//!
//! A second Ctrl+C means the user is done waiting for a clean stop and gets an
//! immediate exit.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use crate::ui::logging::{Level, Logger};

/// Exit code for a run stopped by Ctrl+C — the shell convention of 128 + SIGINT.
pub const EXIT_INTERRUPTED: i32 = 130;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Error)]
#[error("interrupted by Ctrl+C")]
pub struct Interrupted;

/// Install the handler. Failure is not fatal: without it Ctrl+C keeps its
/// default behaviour of killing the process, which is worse but still works.
pub fn install(logger: Arc<Logger>) -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(move || {
        // On Windows every process attached to the console is signalled, so a
        // Ctrl+C aimed at a child arrives here as well. While one owns the
        // console the keypress is not ours to act on.
        if PAUSED.load(Ordering::SeqCst) {
            return;
        }
        if REQUESTED.swap(true, Ordering::SeqCst) {
            logger.write(Level::Warn, "second Ctrl+C, exiting immediately");
            std::process::exit(EXIT_INTERRUPTED);
        }
        logger.write(Level::Warn, "Ctrl+C received, stopping at the next step");
        eprintln!("\nstopping... press Ctrl+C again to quit immediately");
    })
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
