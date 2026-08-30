//! Data and decisions, with nothing underneath them.
//!
//! Every module here is reachable from a test with no disk, no network and no
//! subprocess: what it needs arrives as an argument and what it decides comes
//! back as a value. Nothing in this layer may import from another one.

pub mod assets;
pub mod packages;
pub mod paths;
pub mod preflight;
pub mod state;
