//! ProxSpace — a Proxmark3 development environment for Windows.
//!
//! One binary that provisions an msys2/UCRT64 toolchain next to itself and runs
//! a shell in it, replacing the batch and shell scripts of the original
//! ProxSpace (`runme64.bat`, `setup/setup.cmd`, `09-proxspace_setup.post` and
//! everything in `setup/bin/`).
//!
//! The logic lives in this library rather than in `main.rs` so that the
//! integration tests under `tests/` can drive it directly instead of only
//! through the command line.
//!
//! Module map:
//!
//! - [`paths`] — where everything is on disk (always next to the executable)
//! - [`http`] — the only way out to the network
//! - [`download`] — fetching a file over it, resumably
//! - [`archive`] — unpacking what was fetched
//! - [`assets`] — the files ProxSpace puts inside that tree
//! - [`msys2`] — which msys2 this build installs, and getting it intact
//! - [`preflight`] — checks run before the environment is touched
//! - [`state`] — the install pipeline and its persisted state
//! - [`ui`] — every message the user sees
//! - [`logging`] — the log file those messages are mirrored to
//! - [`interrupt`] — Ctrl+C handling for long operations
//! - [`cli`] — the command tree

pub mod archive;
pub mod assets;
pub mod cli;
pub mod download;
pub mod http;
pub mod interrupt;
pub mod logging;
pub mod msys2;
pub mod paths;
pub mod preflight;
pub mod state;
pub mod ui;

/// Version of this binary, as recorded in the state file and reported by `info`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
