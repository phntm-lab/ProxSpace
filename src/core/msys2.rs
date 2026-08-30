//! Which msys2 this build installs.
//!
//! Four constants and the shape they are handed around in. Nothing here reads
//! a disk or a network: the version is what an installed tree is compared
//! against, and comparing it must not depend on either.

use std::path::PathBuf;

use crate::core::paths::Paths;
use crate::core::versions::file_name_of;

/// Datestamp of the base archive this build installs, in the `YYYYMMDD` form
/// upstream names its archives with.
///
/// This is the version an installed tree is compared against, so bumping msys2
/// is a matter of editing this block — and only this block — as a unit:
///
/// 1. Pick the newest `msys2-base-x86_64-<datestamp>.tar.xz` from
///    <https://repo.msys2.org/distrib/x86_64/> and put its datestamp here.
/// 2. Point [`MSYS2_URL`] at that file.
/// 3. Download it, hash it, and put the hash in [`MSYS2_SHA256`], following the
///    note there about cross-checking a second mirror.
/// 4. Leave [`MSYS2_MIN_COMPATIBLE`] where it is unless the new runtime cannot
///    be reached from the old ones by `pacman -Syuu`.
///
/// Versions are ordered by comparing these strings, which works only while the
/// datestamp form holds; the tests below check the constants against each other
/// and against that form.
pub const MSYS2_VERSION: &str = "20260611";

/// Where the archive for [`MSYS2_VERSION`] is downloaded from. The file name is
/// parsed back out of this URL, so it has to keep the upstream name.
pub const MSYS2_URL: &str =
    "https://mirror.msys2.org/distrib/x86_64/msys2-base-x86_64-20260611.tar.xz";

/// sha256 of the archive at [`MSYS2_URL`].
///
/// Computed from the file itself and cross-checked against a second mirror.
/// Upstream publishes no checksum file alongside the archive, so this constant
/// is the only thing standing between a corrupted or substituted download and
/// an install; it must be recomputed whenever [`MSYS2_URL`] changes.
pub const MSYS2_SHA256: &str = "a2d047e8ee213c3c6a49a8de427eb1069df12207c0422ff1b3cbb5c905c34221";

/// Oldest msys2 version that `pacman -Syuu` can still bring fully up to date.
/// Bump ONLY when upstream breaks the upgrade path from older runtimes:
/// users below this version get a full reinstall instead of an in-place upgrade.
pub const MSYS2_MIN_COMPATIBLE: &str = "20260611";

/// The msys2 subsystem ProxSpace runs in. It decides which prefix
/// (`/ucrt64`) is on `$PATH` and which package set the environment is built
/// from; the original used `MINGW64`, this port moved to UCRT64.
pub const MSYSTEM: &str = "UCRT64";

/// Which base archive to install: the constants above, in a form that can be
/// pointed elsewhere.
///
/// The indirection exists for the tests. Provisioning is the part of this
/// module worth testing — what the state file says after a failure halfway
/// through, what a second run does — and a function wired directly to
/// [`MSYS2_URL`] could only be tested by downloading fifty megabytes and
/// hoping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSource {
    pub url: String,
    pub sha256: String,
    pub version: String,
}

impl ArchiveSource {
    /// The archive this build of ProxSpace installs.
    pub fn msys2() -> ArchiveSource {
        ArchiveSource {
            url: MSYS2_URL.to_string(),
            sha256: MSYS2_SHA256.to_string(),
            version: MSYS2_VERSION.to_string(),
        }
    }

    /// Name the archive is saved under, taken from the URL so the two cannot
    /// drift apart.
    pub fn file_name(&self) -> &str {
        file_name_of(&self.url)
    }

    /// Where it is downloaded to: next to the binary, like everything else. It
    /// exists only between the download and the unpacking.
    pub fn archive_path(&self, paths: &Paths) -> PathBuf {
        paths.base().join(self.file_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::versions::version_from_url;

    #[test]
    fn the_constants_agree_with_each_other() {
        assert_eq!(version_from_url(MSYS2_URL), Some(MSYS2_VERSION));
        assert_eq!(
            ArchiveSource::msys2().file_name(),
            format!("msys2-base-x86_64-{MSYS2_VERSION}.tar.xz")
        );
        for version in [MSYS2_VERSION, MSYS2_MIN_COMPATIBLE] {
            assert_eq!(
                version.len(),
                8,
                "versions are compared as strings, so they must all be datestamps"
            );
            assert!(version.bytes().all(|byte| byte.is_ascii_digit()));
        }
        assert_eq!(MSYS2_SHA256.len(), 64);
        assert!(MSYS2_SHA256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            MSYS2_MIN_COMPATIBLE <= MSYS2_VERSION,
            "the oldest supported version cannot be newer than the shipped one"
        );
    }
}
