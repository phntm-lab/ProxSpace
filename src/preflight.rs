//! Checks that must pass before the environment is touched.
//!
//! Port of the original `setup/startup_checks.sh`, which tested the install
//! path against `^[a-zA-Z0-9\/\._\-]+$` and, on failure, printed one line and
//! slept forever. The character class is kept (widened by `\` and `:`, which a
//! Windows path needs), but each rejection now says which character broke it —
//! "special characters" told the user nothing about their `C:\Program Files`.
//!
//! The restriction is not cosmetic: the msys2 toolchain, `make`, `pacman` and
//! the proxmark3 build scripts all pass this path through shell word splitting
//! and unquoted makefile variables.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::paths::Paths;

/// Below this the install is refused: msys2 plus the ProxSpace package set,
/// the pacman download cache and one proxmark3 build tree do not fit.
pub const REQUIRED_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Below this the install proceeds with a warning — it fits, but leaves no room
/// for a second checkout or a firmware build.
pub const RECOMMENDED_FREE_BYTES: u64 = 15 * 1024 * 1024 * 1024;

/// Why an install path was rejected. Each variant exists to produce a message
/// the user can act on, rather than a generic "invalid path".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    Empty,
    Whitespace,
    NonAscii(char),
    Bracket(char),
    Other(char),
}

impl std::fmt::Display for PathProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathProblem::Empty => write!(f, "the path is empty"),
            PathProblem::Whitespace => write!(
                f,
                "the path contains a space; msys2 build scripts do not quote it \
                 and the build will fail in confusing ways"
            ),
            PathProblem::NonAscii(c) => write!(
                f,
                "the path contains the non-ASCII character `{c}`; the msys2 \
                 toolchain only handles ASCII paths reliably"
            ),
            PathProblem::Bracket(c) => write!(
                f,
                "the path contains `{c}`; brackets are shell metacharacters and \
                 break the build scripts"
            ),
            PathProblem::Other(c) => write!(f, "the path contains the unsupported character `{c}`"),
        }
    }
}

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

/// Characters an install path may consist of. Deliberately narrow: anything
/// outside this set has to survive Windows, cygwin path translation and shell
/// expansion unchanged, and most of it does not.
fn is_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '\\' | ':' | '.' | '_' | '-')
}

pub fn validate_install_path(path: &str) -> Result<(), PathProblem> {
    if path.is_empty() {
        return Err(PathProblem::Empty);
    }
    for c in path.chars() {
        if is_allowed(c) {
            continue;
        }
        return Err(match c {
            c if c.is_whitespace() => PathProblem::Whitespace,
            '(' | ')' | '[' | ']' | '{' | '}' => PathProblem::Bracket(c),
            c if !c.is_ascii() => PathProblem::NonAscii(c),
            c => PathProblem::Other(c),
        });
    }
    Ok(())
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

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_realistic_paths() {
        for path in [
            r"C:\ProxSpace",
            r"C:\tools\proxspace-3.11",
            r"D:\dev\pm3_env\ProxSpace",
            "/c/ProxSpace",
            r"C:\a.b\c-d\e_f",
        ] {
            assert!(validate_install_path(path).is_ok(), "rejected `{path}`");
        }
    }

    #[test]
    fn reports_the_specific_problem() {
        let cases: [(&str, PathProblem); 8] = [
            ("", PathProblem::Empty),
            (r"C:\Program Files\ProxSpace", PathProblem::Whitespace),
            ("C:\\dev\tProxSpace", PathProblem::Whitespace),
            (r"C:\Проекты\ProxSpace", PathProblem::NonAscii('П')),
            (r"C:\dev\ProxSpace (1)", PathProblem::Whitespace),
            (r"C:\dev\ProxSpace(1)", PathProblem::Bracket('(')),
            (r"C:\dev\[old]\ProxSpace", PathProblem::Bracket('[')),
            (r"C:\dev\Prox&Space", PathProblem::Other('&')),
        ];
        for (path, expected) in cases {
            assert_eq!(
                validate_install_path(path),
                Err(expected),
                "wrong verdict for `{path}`"
            );
        }
    }

    #[test]
    fn the_first_offending_character_wins() {
        // `(` comes before the space, so the bracket is reported.
        assert_eq!(
            validate_install_path(r"C:\dev\(a b)"),
            Err(PathProblem::Bracket('('))
        );
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
    }

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
