//! Finding the base directory ProxSpace works in.
//!
//! The layout itself lives in [`crate::core::paths`]; this is the part that
//! has to ask the operating system where the executable is and what the
//! filesystem calls a path.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::paths::Paths;

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

/// Resolve the base directory: `--dir` when given, otherwise the directory
/// containing the executable. Using the executable's own location (rather
/// than the current working directory) is what makes double-clicking from
/// Explorer, launching via `PATH` and `cargo run` all behave the same.
pub fn discover(dir_override: Option<&Path>) -> Result<Paths, PathsError> {
    match dir_override {
        Some(dir) => from_dir(dir),
        None => {
            let exe = env::current_exe().map_err(PathsError::ExeLocation)?;
            let parent = exe
                .parent()
                .ok_or_else(|| PathsError::ExeHasNoParent(exe.clone()))?;
            Ok(Paths::new(absolutize(parent)?))
        }
    }
}

/// Base directory taken from an explicit path. The directory must already
/// exist — creating it silently would hide typos in `--dir`.
pub fn from_dir(dir: &Path) -> Result<Paths, PathsError> {
    if !dir.exists() {
        return Err(PathsError::DirMissing(dir.to_path_buf()));
    }
    if !dir.is_dir() {
        return Err(PathsError::DirNotADirectory(dir.to_path_buf()));
    }
    Ok(Paths::new(absolutize(dir)?))
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
        let paths = from_dir(dir.path()).unwrap();
        let text = paths.base().to_string_lossy().to_string();

        assert!(!text.starts_with(EXTENDED_PREFIX), "not stripped: {text}");
        assert!(
            crate::core::preflight::validate_install_path(&text).is_ok(),
            "rejected: {text}"
        );
    }

    #[test]
    fn missing_dir_override_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("proxspace-does-not-exist");
        assert!(matches!(from_dir(&missing), Err(PathsError::DirMissing(_))));
    }

    #[test]
    fn a_file_is_not_a_valid_base_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            from_dir(&file),
            Err(PathsError::DirNotADirectory(_))
        ));
    }
}
