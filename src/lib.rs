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
//! The modules sit in layers, and a layer may only name itself or a layer
//! further in:
//!
//! - [`core`] — data and decisions: where things go, what the state file means,
//!   which packages and which msys2 this build installs, what an update does to
//!   a tree. Reachable with no disk, no network and no subprocess, and it names
//!   no other layer
//! - [`ui`] — every message the user sees, the log it is mirrored to, and
//!   Ctrl+C. Names no other layer either
//! - [`ports`] — the two ways out of the process that have a second
//!   implementation in tests: running another program, and the network
//! - [`infra`] — the adapters behind those traits, and everything else that
//!   touches the disk or the msys2 tree
//! - [`app`] — what each command does, stage by stage
//! - [`cli`] — the command tree clap parses, and the dispatcher
//!
//! Several module names appear in two layers — `assets`, `paths`, `preflight`,
//! `state`, `pacman`, `msys2`, `http`. That is the split rather than a
//! duplicate: in [`core`] the module says what the answer is, in [`infra`] it
//! reads or writes the file that carries it.
//!
//! `core` and `ui` are peers at the innermost rank and neither may name the
//! other: the decisions must not know how they are shown, and the screen must
//! not know what is being decided. `tests/layers.rs` reads `src/` and enforces
//! all of this, counting a `crate::` path in a doc link exactly as it counts a
//! `use`.

pub mod app;
pub mod cli;
pub mod core;
pub mod infra;
pub mod ports;
pub mod ui;

/// Version of this binary, as recorded in the state file and reported by `info`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
