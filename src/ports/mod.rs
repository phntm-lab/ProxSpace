//! The ways out of the process, as traits.
//!
//! Two of them, and both exist because the tests need a second implementation:
//! running another program and talking to the network. Everything else that
//! leaves the process does so through [`crate::infra`] directly.

pub mod command;
pub mod http;
