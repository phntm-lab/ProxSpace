//! What one install run is putting in place.
//!
//! The archive, the package list and the mounts arrive as values rather than
//! being read from the constants directly, so that the pipeline can be tested
//! against a small archive and a three-line list instead of five gigabytes.

use crate::core::fstab::Mounts;
use crate::core::msys2::{ArchiveSource, MSYS2_MIN_COMPATIBLE};
use crate::core::packages::{PackageList, PackagesError};
use crate::core::paths::Paths;

/// What this run is installing.
///
/// The archive and the list are parameters rather than constants for the same
/// reason [`ArchiveSource`] is: the pipeline is the part worth testing, and a
/// pipeline wired directly to the shipped list could only be tested by
/// installing five gigabytes.
pub struct Plan {
    pub source: ArchiveSource,
    /// Oldest tree `pacman -Syuu` can still bring up to `source.version`.
    /// Alongside the source for the same reason: it is part of what this build
    /// ships, and the update matrix is worth testing without a real tree.
    pub min_compatible: String,
    pub list: PackageList,
    pub mounts: Mounts,
    /// Install every package in the list again, whether or not it is already
    /// there. What `install --force` sets, and what a moved installation asks
    /// for.
    pub force: bool,
}

impl Plan {
    /// What this build of ProxSpace installs.
    pub fn shipped(paths: &Paths) -> Result<Plan, PackagesError> {
        Ok(Plan {
            source: ArchiveSource::msys2(),
            min_compatible: MSYS2_MIN_COMPATIBLE.to_string(),
            list: PackageList::shipped()?,
            mounts: Mounts::for_paths(paths),
            force: false,
        })
    }

    pub fn forced(mut self, force: bool) -> Plan {
        self.force = force;
        self
    }
}
