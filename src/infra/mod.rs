//! Adapters: the code that actually touches the disk, the network and other
//! programs, on the near side of the traits in [`crate::ports`].

pub mod archive;
pub mod assets;
pub mod download;
pub mod msys2;
pub mod pacman;
pub mod paths;
pub mod preflight;
pub mod state;
