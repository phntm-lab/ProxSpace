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
//! # Layers
//!
//! The modules are grouped into layers, and a layer may only use the layers
//! below it:
//!
//! - [`core`] — data and decisions, reachable with no disk, network or
//!   subprocess; uses nothing else here
//! - [`ui`] — every message the user sees, the log it is mirrored to, and
//!   Ctrl+C; uses nothing else here
//! - [`ports`] — the traits for leaving the process: running another program,
//!   talking to the network
//! - [`infra`] — the adapters behind those traits, and everything else that
//!   touches the disk or the msys2 tree
//! - [`app`] — what each command does, stage by stage
//! - [`cli`] — the command tree clap parses, and the dispatcher

pub mod app;
pub mod cli;
pub mod core;
pub mod infra;
pub mod ports;
pub mod ui;

/// Version of this binary, as recorded in the state file and reported by `info`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
