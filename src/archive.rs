//! Unpacking the msys2 base archive.
//!
//! Three things make this more than a call to `tar::Archive::unpack`:
//!
//! - **no intermediate `.tar`.** The xz stream is decoded straight into the tar
//!   reader, so unpacking costs the disk space of the result and nothing more;
//! - **the top-level directory is stripped.** Inside the archive everything
//!   lives under a single root (`msys64/` for the msys2 base archive), and what
//!   we want on disk is our own `msys2/` holding its contents;
//! - **the tree appears whole or not at all.** Files go into a `.partial`
//!   sibling that is renamed into place at the very end. Sixteen thousand files
//!   take a while, and a run interrupted in the middle must not leave something
//!   that looks like a working msys2 — the next run wipes the leftovers and
//!   starts over.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Component, Path, PathBuf};

use tar::Archive;
use thiserror::Error;
use xz2::read::XzDecoder;

use crate::interrupt::{self, Interrupted};
use crate::ui::Ui;

/// Suffix of the directory the archive is unpacked into before it is renamed
/// into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// How many times a tree removal is attempted before it is reported as a
/// failure.
const REMOVE_ATTEMPTS: usize = 5;

/// Gap before the second attempt; each further attempt waits a multiple of it,
/// so the whole thing gives up after about a second and a half rather than
/// hanging on a file nothing is going to let go of.
const REMOVE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("cannot open the archive `{path}`")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the archive is damaged or not a .tar.xz; delete `{path}` and run the command again to download it afresh"
    )]
    Corrupt {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "cannot remove `{path}`: `{blocker}` is in use by another program
           close the shell, editor or build using it — or the antivirus scanning it —          and run the command again"
    )]
    Blocked {
        path: PathBuf,
        blocker: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot remove `{path}`")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{0}` already exists; remove it before unpacking a new one")]
    DestinationExists(PathBuf),
    #[error("the archive is empty")]
    Empty,
    #[error(
        "the archive holds more than one top-level directory (`{first}` and `{other}`), \
         so there is nothing to unwrap; this is not the expected msys2 base archive"
    )]
    MixedRoots { first: String, other: String },
    #[error("the archive contains an entry that would be written outside the target: `{0}`")]
    UnsafePath(String),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// Files and directories written, not counting the stripped root itself.
    pub entries: u64,
    /// Name of the top-level directory that was stripped, e.g. `msys64`.
    pub root: String,
}

/// Where a half-finished extraction of `destination` lives.
pub fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(PARTIAL_SUFFIX);
    PathBuf::from(name)
}

/// Unpack a `.tar.xz` into `destination`, dropping the archive's single
/// top-level directory.
///
/// `destination` must not exist: an existing tree is the caller's to remove,
/// because deciding whether a msys2 install may be thrown away is not a
/// decision an unpacking function gets to make.
pub fn extract_stripping_root(
    archive: &Path,
    destination: &Path,
    ui: &Ui,
    message: &str,
) -> Result<Extracted, ExtractError> {
    if destination.exists() {
        return Err(ExtractError::DestinationExists(destination.to_path_buf()));
    }

    let staging = partial_path(destination);
    // Leftovers from a run that died mid-unpack. They are worthless by
    // definition — the rename never happened — so they go without asking.
    if staging.exists() {
        ui.detail(&format!(
            "removing the leftovers of an earlier attempt in `{}`",
            staging.display()
        ));
        remove_tree(&staging)?;
    }
    fs::create_dir_all(&staging).map_err(|source| ExtractError::Io {
        path: staging.clone(),
        source,
    })?;

    match unpack_into(archive, &staging, ui, message) {
        Ok(extracted) => {
            fs::rename(&staging, destination).map_err(|source| ExtractError::Io {
                path: destination.to_path_buf(),
                source,
            })?;
            Ok(extracted)
        }
        Err(error) => {
            // Nothing partially unpacked survives: leaving it would make the
            // next run's `remove_tree` the only thing standing between the user
            // and a tree that is half one version and half another.
            let _ = remove_tree(&staging);
            Err(error)
        }
    }
}

fn unpack_into(
    archive: &Path,
    staging: &Path,
    ui: &Ui,
    message: &str,
) -> Result<Extracted, ExtractError> {
    let file = File::open(archive).map_err(|source| ExtractError::Open {
        path: archive.to_path_buf(),
        source,
    })?;
    // The xz decoder reads from a buffered file and the tar reader reads from
    // the decoder: one pass, no temporary `.tar` anywhere.
    let mut tar = Archive::new(XzDecoder::new(BufReader::new(file)));
    // Unix modes and ownership mean nothing on Windows, and asking for them
    // only produces failures on files that are otherwise fine.
    tar.set_preserve_permissions(false);
    tar.set_unpack_xattrs(false);

    // The entry count is not known until the stream has been read, and reading
    // it twice would mean decompressing it twice — so the progress is a running
    // count rather than a percentage.
    let bar = ui.progress_items(None, message);

    let mut root: Option<String> = None;
    let mut entries = 0u64;

    let iterator = tar.entries().map_err(|source| ExtractError::Corrupt {
        path: archive.to_path_buf(),
        source,
    })?;

    for entry in iterator {
        interrupt::check().inspect_err(|_| bar.abandon())?;

        let mut entry = entry.map_err(|source| ExtractError::Corrupt {
            path: archive.to_path_buf(),
            source,
        })?;
        let path = entry
            .path()
            .map_err(|source| ExtractError::Corrupt {
                path: archive.to_path_buf(),
                source,
            })?
            .into_owned();

        let (entry_root, rest) = split_root(&path)?;
        match &root {
            None => root = Some(entry_root.clone()),
            Some(first) if *first != entry_root => {
                return Err(ExtractError::MixedRoots {
                    first: first.clone(),
                    other: entry_root,
                });
            }
            Some(_) => {}
        }

        // The root directory entry itself becomes the destination directory,
        // which already exists.
        let Some(rest) = rest else { continue };

        let target = staging.join(&rest);
        // Not every archive lists a directory before the files in it, and
        // unpacking an entry does not create its parents.
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        entry.unpack(&target).map_err(|source| ExtractError::Io {
            path: target.clone(),
            source,
        })?;
        entries += 1;
        bar.inc(1);
    }

    bar.finish_and_clear();

    match root {
        Some(root) => Ok(Extracted { entries, root }),
        None => Err(ExtractError::Empty),
    }
}

/// Split an archive path into its top-level directory and the rest, refusing
/// anything that does not stay inside that directory.
///
/// `..` and absolute paths are the classic way a hostile archive writes outside
/// the directory it is unpacked into; a path with no directory at all cannot
/// have its root stripped and means the archive is not shaped as expected.
fn split_root(path: &Path) -> Result<(String, Option<PathBuf>), ExtractError> {
    let unsafe_path = || ExtractError::UnsafePath(path.display().to_string());

    let mut components = path.components();
    let root = loop {
        match components.next() {
            Some(Component::Normal(name)) => break name.to_string_lossy().into_owned(),
            // Some producers write paths as `./dir/file`; the prefix says
            // nothing and can be dropped.
            Some(Component::CurDir) => {}
            // No components at all, or a leading `/`, `..`, `C:` — none of
            // which can be unwrapped into a directory of our choosing.
            _ => return Err(unsafe_path()),
        }
    };

    let mut rest = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => rest.push(part),
            // `.` is harmless but pointless; everything else escapes.
            Component::CurDir => {}
            _ => return Err(unsafe_path()),
        }
    }

    Ok((root, (!rest.as_os_str().is_empty()).then_some(rest)))
}

/// Delete a directory tree, tolerating one that is not there.
///
/// Deleting sixteen thousand files on Windows fails for reasons that have
/// nothing to do with the caller, so a single `remove_dir_all` is not enough:
///
/// - a **read-only file** cannot be deleted at all, and the attribute survives
///   whatever put it there. Clearing it is the only way past;
/// - an **antivirus or the search indexer** opens files as they appear and lets
///   go a moment later, which shows up as a sharing violation that is gone by
///   the next attempt;
/// - a **program still using the tree** — a shell, an editor, a build — holds
///   its file for as long as it runs, and no amount of waiting helps. That one
///   is reported by name, because "cannot remove msys2" is not something anyone
///   can act on.
pub fn remove_tree(path: &Path) -> Result<(), ExtractError> {
    for attempt in 1..=REMOVE_ATTEMPTS {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt == REMOVE_ATTEMPTS => return Err(removal_failed(path, error)),
            Err(_) => {
                interrupt::check()?;
                // Whatever is left of the tree at this point is what the failed
                // attempt could not get to; the attribute is the one cause that
                // never clears itself.
                clear_read_only(path);
                std::thread::sleep(REMOVE_BACKOFF * attempt as u32);
            }
        }
    }
    Ok(())
}

/// Turn a failed removal into something that names a cause.
fn removal_failed(path: &Path, source: io::Error) -> ExtractError {
    match blocking_file(path) {
        Some(blocker) => ExtractError::Blocked {
            path: path.to_path_buf(),
            blocker,
            source,
        },
        None => ExtractError::Remove {
            path: path.to_path_buf(),
            source,
        },
    }
}

/// Clear the read-only attribute from everything in the tree.
///
/// Best effort throughout: this runs to make the next removal attempt more
/// likely to work, and a file it cannot touch is one the removal will report
/// on its own terms.
fn clear_read_only(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };

    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        // What the mode says afterwards does not matter: the only thing that
        // happens to this file next is that it is deleted.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }

    // Not through a symbolic link: the target is somewhere else entirely, and
    // this tree's removal is no reason to go changing it.
    if metadata.is_dir()
        && !metadata.is_symlink()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            clear_read_only(&entry.path());
        }
    }
}

/// Find a file in the tree that another program is holding open.
///
/// Windows reports a failed directory removal without saying which file stood
/// in the way. Opening a file for writing asks it the same question the
/// deletion asks — is anyone else using this? — and answers it without changing
/// anything, which is why the tree is probed rather than deleted a second time
/// file by file.
fn blocking_file(root: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if let Some(found) = blocking_file(&path) {
                return Some(found);
            }
        } else if let Err(error) = fs::OpenOptions::new().write(true).open(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree with a read-only file in it: `remove_dir_all` alone refuses this
    /// one on Windows, and pacman leaves such files behind.
    #[test]
    fn a_read_only_file_does_not_stop_a_removal() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("msys2");
        fs::create_dir_all(tree.join("etc")).unwrap();

        let file = tree.join("etc").join("locked.conf");
        fs::write(&file, "held").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();

        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
    }

    #[test]
    fn removing_what_is_not_there_is_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        remove_tree(&dir.path().join("never-existed")).unwrap();
    }

    /// The failure the user actually meets: a program is running from inside
    /// the tree, and the message has to name the file rather than the folder.
    #[cfg(windows)]
    #[test]
    fn a_file_another_program_holds_is_named() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("msys2");
        fs::create_dir_all(tree.join("usr").join("bin")).unwrap();
        let held = tree.join("usr").join("bin").join("bash.exe");
        fs::write(&held, "program").unwrap();

        // Sharing nothing is how Windows opens a running executable; a file
        // opened the ordinary way can still be deleted underneath its holder.
        let _handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&held)
            .unwrap();

        let error = remove_tree(&tree).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("bash.exe"),
            "the message should name the file: {message}"
        );
        assert!(matches!(error, ExtractError::Blocked { .. }), "{error:?}");
    }

    #[test]
    fn a_tree_nothing_is_using_has_no_blocking_file() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("msys2");
        fs::create_dir_all(tree.join("usr")).unwrap();
        fs::write(tree.join("usr").join("free.txt"), "nothing holds this").unwrap();

        assert_eq!(blocking_file(&tree), None);
    }

    #[test]
    fn the_staging_directory_sits_next_to_the_destination() {
        assert_eq!(
            partial_path(Path::new(r"C:\ProxSpace\msys2")),
            PathBuf::from(r"C:\ProxSpace\msys2.partial")
        );
    }

    #[test]
    fn a_root_and_its_contents_are_split() {
        let (root, rest) = split_root(Path::new("msys64/usr/bin/bash.exe")).unwrap();
        assert_eq!(root, "msys64");
        assert_eq!(rest, Some(PathBuf::from("usr/bin/bash.exe")));
    }

    #[test]
    fn the_root_entry_itself_has_no_remainder() {
        assert_eq!(
            split_root(Path::new("msys64/")).unwrap(),
            ("msys64".into(), None)
        );
        assert_eq!(
            split_root(Path::new("msys64")).unwrap(),
            ("msys64".into(), None)
        );
    }

    #[test]
    fn a_current_directory_component_is_ignored() {
        let (root, rest) = split_root(Path::new("./msys64/etc/fstab")).unwrap();
        // A leading `./` is the root's own component in tar paths written by
        // some producers; what matters is that nothing escapes.
        assert_eq!(root, "msys64");
        assert_eq!(rest, Some(PathBuf::from("etc/fstab")));
    }

    #[test]
    fn escaping_paths_are_refused() {
        for path in ["msys64/../../evil", "/etc/passwd", "../evil", ""] {
            assert!(
                matches!(
                    split_root(Path::new(path)),
                    Err(ExtractError::UnsafePath(_))
                ),
                "accepted `{path}`"
            );
        }
    }

    #[test]
    fn removing_a_tree_that_is_not_there_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remove_tree(&dir.path().join("nothing")).is_ok());
    }
}
