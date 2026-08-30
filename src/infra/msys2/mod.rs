//! Running msys2 programs directly, without a login shell.
//!
//! Which msys2 this build installs is decided in [`crate::core::msys2`]; the
//! submodules here get the archive, patch the tree and reach into it.

pub mod archive;
pub mod fstab;
pub mod procs;
pub mod shell;
pub mod userdb;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::core::msys2::MSYSTEM;

/// Environment for an msys2 program started directly as a Windows process,
/// without going through a login shell.
///
/// `PATH` is replaced rather than extended, and that is the whole point.
/// Every msys2 program loads `msys-2.0.dll` by name, so a Cygwin or a second
/// msys2 installation earlier on the user's `PATH` gets loaded instead of ours,
/// and the two runtimes refuse to share a process ("shared region is corrupted"
/// / "heap version mismatch"). The symptom is an install that fails on a
/// machine with Git for Windows on it and nowhere else. The Windows system
/// directories stay on the path because that is where the OS keeps the DLLs
/// every process needs.
pub fn tool_env(tree: &Path) -> Vec<(OsString, OsString)> {
    // The order a UCRT64 login shell ends up with: the subsystem's own prefix
    // first, then the msys2 core. Anything built for UCRT64 — python and the
    // proxmark3 toolchain — needs its DLLs found before the msys2 ones.
    let mut path = OsString::from(tree.join("ucrt64/bin"));
    path.push(";");
    path.push(tree.join("usr/bin"));
    for directory in windows_system_dirs() {
        path.push(";");
        path.push(directory);
    }
    vec![
        (OsString::from("PATH"), path),
        (OsString::from("MSYSTEM"), OsString::from(MSYSTEM)),
    ]
}

/// Where Windows keeps its own DLLs and tools. Read from the environment
/// rather than hardcoded to `C:\Windows`: it is not always there.
fn windows_system_dirs() -> Vec<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    vec![root.join("System32"), root]
}
