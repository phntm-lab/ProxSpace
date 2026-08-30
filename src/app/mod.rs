//! What the commands do, stage by stage.
//!
//! A module here orchestrates: it asks [`crate::core`] what to do, uses
//! [`crate::infra`] to do it, and reports through [`crate::ui`].

pub mod autobuild;
pub mod clean;
pub mod info;
pub mod install;
pub mod mirrors;
pub mod release;
pub mod update;
