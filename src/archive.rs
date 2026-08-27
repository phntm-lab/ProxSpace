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
pub fn remove_tree(path: &Path) -> Result<(), ExtractError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ExtractError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
