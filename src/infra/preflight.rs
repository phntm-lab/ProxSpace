//! Checks that need the disk to answer: can we write here, and is there room.
//!
//! What counts as a usable install path is decided in
//! [`crate::core::preflight`]; this is the half that has to look.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::paths::Paths;
use crate::core::preflight::{
    PathProblem, RECOMMENDED_FREE_BYTES, REQUIRED_FREE_BYTES, human_bytes, validate_install_path,
};

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error("install path `{path}` is not usable: {problem}")]
    InvalidPath { path: String, problem: PathProblem },
    #[error("cannot write to `{path}`")]
    NotWritable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "not enough free disk space on `{path}`: {available} available, at least {required} needed"
    )]
    NotEnoughSpace {
        path: PathBuf,
        available: String,
        required: String,
    },
}

/// Everything preflight found worth mentioning but not worth refusing over.
#[derive(Debug, Default)]
pub struct Preflight {
    pub warnings: Vec<String>,
}

/// Run every check against the resolved base directory.
pub fn run(paths: &Paths) -> Result<Preflight, PreflightError> {
    let base = paths.base();
    let text = base.to_string_lossy();

    validate_install_path(&text).map_err(|problem| PreflightError::InvalidPath {
        path: text.to_string(),
        problem,
    })?;

    check_writable(base)?;

    let mut preflight = Preflight::default();
    match free_space(base) {
        Some(available) if available < REQUIRED_FREE_BYTES => {
            return Err(PreflightError::NotEnoughSpace {
                path: base.to_path_buf(),
                available: human_bytes(available),
                required: human_bytes(REQUIRED_FREE_BYTES),
            });
        }
        Some(available) if available < RECOMMENDED_FREE_BYTES => {
            preflight.warnings.push(format!(
                "only {} free on `{}`; {} is recommended to build proxmark3 comfortably",
                human_bytes(available),
                base.display(),
                human_bytes(RECOMMENDED_FREE_BYTES)
            ));
        }
        Some(_) => {}
        None => preflight.warnings.push(format!(
            "could not determine free disk space on `{}`; skipping that check",
            base.display()
        )),
    }

    Ok(preflight)
}

/// Probe write access by actually writing, rather than reading permissions:
/// on Windows the effective answer depends on ACLs, the process token and
/// controlled-folder-access policies that a metadata check does not see.
fn check_writable(dir: &Path) -> Result<(), PreflightError> {
    let probe = dir.join(".proxspace-write-test");
    let result = fs::write(&probe, b"proxspace");
    let _ = fs::remove_file(&probe);
    result.map_err(|source| PreflightError::NotWritable {
        path: dir.to_path_buf(),
        source,
    })
}

/// Free space available to this user on the volume holding `dir`, or `None`
/// when the platform cannot answer.
pub fn free_space(dir: &Path) -> Option<u64> {
    // The API needs an existing directory; walk up to the nearest one that is
    // actually on disk so a not-yet-created target still gets an answer.
    let mut candidate = Some(dir);
    while let Some(path) = candidate {
        if path.is_dir() {
            return platform_free_space(path);
        }
        candidate = path.parent();
    }
    None
}

#[cfg(windows)]
fn platform_free_space(dir: &Path) -> Option<u64> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        // Reported per-user: on a volume with quotas this is smaller than the
        // raw free space, which is exactly the number we want to check.
        unsafe fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_bytes_available_to_caller: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = OsStr::new(dir)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // and the out-parameters are valid, writable and correctly sized.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

#[cfg(not(windows))]
fn platform_free_space(_dir: &Path) -> Option<u64> {
    // ProxSpace targets Windows; elsewhere the check degrades to a warning so
    // that the rest of the tool stays testable on other platforms.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_directory_passes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_writable(dir.path()).is_ok());
        // The probe file must not survive the check.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn writable_check_fails_on_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(matches!(
            check_writable(&missing),
            Err(PreflightError::NotWritable { .. })
        ));
    }
}
