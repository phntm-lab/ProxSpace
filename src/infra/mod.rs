//! Adapters: everything that touches the disk, the network or another program.
//!
//! [`process`] and [`http`] sit behind the traits in [`crate::ports`]; the rest
//! are used directly. What any of them should do is decided in
//! [`crate::core`] — these modules only carry it out.

pub mod archive;
pub mod assets;
pub mod download;
pub mod http;
pub mod msys2;
pub mod pacman;
pub mod paths;
pub mod preflight;
pub mod process;
pub mod state;
