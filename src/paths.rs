//! Layout of everything ProxSpace creates on disk.
//!
//! Rule D7 of the design: every file this binary writes lives next to the
//! binary itself. Nothing goes to `%APPDATA%`, `%TEMP%` or the user profile,
//! so the whole environment stays portable — copy the folder, keep the setup.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const MSYS2_DIR: &str = "msys2";
pub const PM3_DIR: &str = "pm3";
pub const BUILDS_DIR: &str = "builds";
pub const STATE_FILE: &str = "proxspace.state.json";
pub const LOG_FILE: &str = "proxspace.log";
pub const LOG_BACKUP_FILE: &str = "proxspace.log.old";
/// Where `info` leaves a copy of its report.
pub const INFO_FILE: &str = "proxspace-info.txt";

/// Prefix Windows puts on canonicalised paths to lift the 260-character limit.
const EXTENDED_PREFIX: &str = r"\\?\";

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("cannot determine the location of the running executable")]
    ExeLocation(#[source] io::Error),
    #[error("the executable path `{0}` has no parent directory")]
    ExeHasNoParent(PathBuf),
    #[error("--dir `{0}` does not exist")]
    DirMissing(PathBuf),
    #[error("--dir `{0}` is not a directory")]
    DirNotADirectory(PathBuf),
    #[error("cannot resolve `{path}`")]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Base directory plus every path derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    base: PathBuf,
}

impl Paths {
    /// Resolve the base directory: `--dir` when given, otherwise the directory
    /// containing the executable. Using the executable's own location (rather
    /// than the current working directory) is what makes double-clicking from
    /// Explorer, launching via `PATH` and `cargo run` all behave the same.
    pub fn discover(dir_override: Option<&Path>) -> Result<Self, PathsError> {
        match dir_override {
            Some(dir) => Self::from_dir(dir),
            None => {
                let exe = env::current_exe().map_err(PathsError::ExeLocation)?;
                let parent = exe
                    .parent()
                    .ok_or_else(|| PathsError::ExeHasNoParent(exe.clone()))?;
                Ok(Self {
                    base: absolutize(parent)?,
                })
            }
        }
    }

    /// Base directory taken from an explicit path. The directory must already
    /// exist — creating it silently would hide typos in `--dir`.
    pub fn from_dir(dir: &Path) -> Result<Self, PathsError> {
        if !dir.exists() {
            return Err(PathsError::DirMissing(dir.to_path_buf()));
        }
        if !dir.is_dir() {
            return Err(PathsError::DirNotADirectory(dir.to_path_buf()));
        }
        Ok(Self {
            base: absolutize(dir)?,
        })
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

/// Make a path absolute, and canonical when the target exists.
///
/// `canonicalize` on Windows returns an extended-length path. That form is
/// correct but leaks into user-facing messages, into `fstab` and into the
/// install-path validation, where its leading backslashes and question mark
/// would be flagged as invalid characters — so strip it back to `C:\...`.
fn absolutize(path: &Path) -> Result<PathBuf, PathsError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|source| PathsError::Resolve {
                path: path.to_path_buf(),
                source,
            })?
            .join(path)
    };

    match absolute.canonicalize() {
        Ok(canonical) => Ok(strip_extended_prefix(canonical)),
        // Not yet on disk: an absolute-but-uncanonical path is still usable.
        Err(_) => Ok(absolute),
    }
}

fn strip_extended_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    // Only the plain drive form is unwrapped. The UNC form must keep its
    // prefix: dropping it would silently change which host is addressed.
    if let Some(rest) = text.strip_prefix(EXTENDED_PREFIX)
        && rest.len() >= 2
        && rest.as_bytes()[1] == b':'
    {
        return PathBuf::from(rest);
    }
    path
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

    #[test]
    fn extended_length_prefix_is_stripped_for_drive_paths() {
        let verbatim = PathBuf::from(format!(r"{EXTENDED_PREFIX}C:\ProxSpace"));
        assert_eq!(
            strip_extended_prefix(verbatim),
            PathBuf::from(r"C:\ProxSpace")
        );
    }

    #[test]
    fn unc_prefix_is_preserved() {
        let unc = PathBuf::from(format!(r"{EXTENDED_PREFIX}UNC\server\share"));
        assert_eq!(strip_extended_prefix(unc.clone()), unc);
    }

    #[test]
    fn plain_paths_are_left_alone() {
        let plain = PathBuf::from(r"C:\ProxSpace");
        assert_eq!(strip_extended_prefix(plain.clone()), plain);
    }

    /// The whole point of stripping: a canonicalised path must still pass the
    /// install-path validation, which rejects `?` and stray backslashes.
    #[test]
    fn a_canonicalised_temp_dir_is_a_usable_install_path() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();
        let text = paths.base().to_string_lossy().to_string();

        assert!(!text.starts_with(EXTENDED_PREFIX), "not stripped: {text}");
        assert!(
            crate::preflight::validate_install_path(&text).is_ok(),
            "rejected: {text}"
        );
    }

    #[test]
    fn missing_dir_override_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("proxspace-does-not-exist");
        assert!(matches!(
            Paths::from_dir(&missing),
            Err(PathsError::DirMissing(_))
        ));
    }

    #[test]
    fn a_file_is_not_a_valid_base_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            Paths::from_dir(&file),
            Err(PathsError::DirNotADirectory(_))
        ));
    }
}
