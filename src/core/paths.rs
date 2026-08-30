//! Layout of everything ProxSpace creates on disk.
//!
//! Rule D7 of the design: every file this binary writes lives next to the
//! binary itself. Nothing goes to `%APPDATA%`, `%TEMP%` or the user profile,
//! so the whole environment stays portable — copy the folder, keep the setup.

use std::path::{Path, PathBuf};

pub const MSYS2_DIR: &str = "msys2";
pub const PM3_DIR: &str = "pm3";
pub const BUILDS_DIR: &str = "builds";
pub const STATE_FILE: &str = "proxspace.state.json";
pub const LOG_FILE: &str = "proxspace.log";
pub const LOG_BACKUP_FILE: &str = "proxspace.log.old";
/// Where `info` leaves a copy of its report.
pub const INFO_FILE: &str = "proxspace-info.txt";

/// Base directory plus every path derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    base: PathBuf,
}

impl Paths {
    /// A base directory that has already been resolved.
    pub fn new(base: PathBuf) -> Paths {
        Paths { base }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The msys2 tree: downloaded, unpacked and patched by later stages.
    pub fn msys2(&self) -> PathBuf {
        self.base.join(MSYS2_DIR)
    }

    /// `$HOME` inside the shell, mounted as `/pm3`. Holds proxmark3 sources.
    pub fn pm3(&self) -> PathBuf {
        self.base.join(PM3_DIR)
    }

    /// Output of `autobuild`, mounted as `/builds` only for that command.
    pub fn builds(&self) -> PathBuf {
        self.base.join(BUILDS_DIR)
    }

    pub fn state_file(&self) -> PathBuf {
        self.base.join(STATE_FILE)
    }

    pub fn log_file(&self) -> PathBuf {
        self.base.join(LOG_FILE)
    }

    pub fn log_backup_file(&self) -> PathBuf {
        self.base.join(LOG_BACKUP_FILE)
    }

    /// The report `info` writes, so that it can be attached to a bug report
    /// rather than copied out of a console.
    pub fn info_file(&self) -> PathBuf {
        self.base.join(INFO_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_hang_off_the_base() {
        let paths = Paths {
            base: PathBuf::from(r"C:\ProxSpace"),
        };
        assert_eq!(paths.msys2(), PathBuf::from(r"C:\ProxSpace\msys2"));
        assert_eq!(paths.pm3(), PathBuf::from(r"C:\ProxSpace\pm3"));
        assert_eq!(paths.builds(), PathBuf::from(r"C:\ProxSpace\builds"));
        assert_eq!(
            paths.state_file(),
            PathBuf::from(r"C:\ProxSpace\proxspace.state.json")
        );
        assert_eq!(
            paths.log_file(),
            PathBuf::from(r"C:\ProxSpace\proxspace.log")
        );
        assert_eq!(
            paths.log_backup_file(),
            PathBuf::from(r"C:\ProxSpace\proxspace.log.old")
        );
        assert_eq!(
            paths.info_file(),
            PathBuf::from(r"C:\ProxSpace\proxspace-info.txt")
        );
    }
}
