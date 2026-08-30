//! The ways out of the process, as traits.
//!
//! Two of them, and both exist for the same reason: the tests need a second
//! implementation. A download is a hundred megabytes and a pacman run is
//! twenty minutes, so the code around them can only be exercised against
//! something that answers instantly.
//!
//! Everything else that leaves the process — reading a file, writing one,
//! looking at what is running — has no second implementation and no trait; it
//! lives among the adapters and is used from there directly.
//!
//! Both traits take the [`crate::ui::Ui`]: what they do takes minutes and has
//! to be watched while it happens, not reported once it is over.

pub mod command;
pub mod http;
