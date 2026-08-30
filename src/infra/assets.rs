//! Putting the built-in assets into the msys2 tree.
//!
//! What the files are lives in [`crate::core::assets`]; this writes them and
//! reports what changed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::assets::assets;
use crate::ui::Ui;

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("cannot create `{path}`")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Put every asset in place under `root`, which must be the msys2 tree.
///
/// Idempotent by comparison, not by overwriting: a file that already holds the
/// right bytes is left alone, so the mtimes in the tree keep meaning something
/// and a run that changes nothing says so. A file that was edited by hand is
/// silently restored — these are ours, and the way to change them is to change
/// the binary.
pub fn install(root: &Path, ui: &Ui) -> Result<Report, AssetError> {
    let mut report = Report::default();

    for asset in assets() {
        let destination = asset.destination(root);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| AssetError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        if is_already_there(&destination, &asset.contents) {
            report.unchanged += 1;
        } else {
            fs::write(&destination, asset.contents.as_bytes()).map_err(|source| {
                AssetError::Write {
                    path: destination.clone(),
                    source,
                }
            })?;
            ui.detail(&format!("wrote `{}`", destination.display()));
            report.written.push(destination.clone());
        }

        if asset.executable {
            make_executable(&destination).map_err(|source| AssetError::Write {
                path: destination,
                source,
            })?;
        }
    }

    Ok(report)
}

/// What [`install`] did, for the caller to log or ignore.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub written: Vec<PathBuf>,
    pub unchanged: usize,
}

impl Report {
    pub fn changed_anything(&self) -> bool {
        !self.written.is_empty()
    }
}

fn is_already_there(path: &Path, contents: &str) -> bool {
    // An unreadable file counts as different: the write that follows is the
    // best chance of getting a usable one, and it will report the real error.
    fs::read(path).is_ok_and(|existing| existing == contents.as_bytes())
}

/// Mark a script as runnable.
///
/// On Windows this is a no-op, and deliberately so: the tree is mounted
/// `noacl` (`fstab.rs`), under which cygwin does not consult NTFS permissions
/// at all and calls a file executable when it starts with `#!` — which every
/// script here does. Faking a POSIX mode through the Windows API would change
/// nothing about what the shell sees.
#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use crate::core::assets::{Asset, BIN_DIR, HOOK_PATH, NSSWITCH_PATH, PACKAGES_PATH};

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::ui::logging::Logger::disabled()),
        )
    }

    fn asset_named(name: &str) -> Asset {
        assets()
            .into_iter()
            .find(|asset| asset.name == name)
            .unwrap_or_else(|| panic!("no asset named {name}"))
    }

    #[test]
    fn installing_puts_every_asset_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let report = install(root, &silent_ui()).unwrap();

        assert_eq!(report.written.len(), assets().len());
        assert_eq!(report.unchanged, 0);
        for asset in assets() {
            let destination = asset.destination(root);
            assert_eq!(
                fs::read_to_string(&destination).unwrap(),
                asset.contents,
                "{} was written wrong",
                destination.display()
            );
        }
        assert!(root.join(HOOK_PATH).is_file());
        assert!(root.join(NSSWITCH_PATH).is_file());
        assert!(root.join(PACKAGES_PATH).is_file());
        assert!(root.join(BIN_DIR).join("pm3").is_file());
    }

    #[test]
    fn a_second_install_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &silent_ui()).unwrap();

        let second = install(dir.path(), &silent_ui()).unwrap();

        assert!(!second.changed_anything());
        assert_eq!(second.unchanged, assets().len());
    }

    #[test]
    fn an_edited_asset_is_put_back() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &silent_ui()).unwrap();
        let hook = dir.path().join(HOOK_PATH);
        fs::write(&hook, b"rm -rf /\n").unwrap();

        let report = install(dir.path(), &silent_ui()).unwrap();

        assert_eq!(report.written, vec![hook.clone()]);
        assert_eq!(
            fs::read_to_string(&hook).unwrap(),
            asset_named("09-proxspace_setup.post").contents
        );
    }
}
