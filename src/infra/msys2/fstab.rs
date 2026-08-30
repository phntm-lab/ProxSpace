//! Writing the mount table into the tree.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::fstab::{FSTAB_PATH, Mounts, render};
use crate::ui::Ui;

#[derive(Debug, Error)]
pub enum FstabError {
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

/// Write `/etc/fstab` into the tree, returning whether anything changed.
pub fn install(root: &Path, mounts: &Mounts, ui: &Ui) -> Result<bool, FstabError> {
    let destination = root.join(FSTAB_PATH);
    let contents = render(mounts);

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| FstabError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    if fs::read(&destination).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(false);
    }

    fs::write(&destination, contents.as_bytes()).map_err(|source| FstabError::Write {
        path: destination.clone(),
        source,
    })?;
    ui.detail(&format!("wrote `{}`", destination.display()));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    const BASIC: &str = include_str!("../../../tests/fixtures/fstab-basic");

    fn fixture(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::ui::logging::Logger::disabled()),
        )
    }

    fn mounts(builds: bool) -> Mounts {
        Mounts {
            pm3: PathBuf::from(r"C:\ProxSpace\pm3"),
            builds: builds.then(|| PathBuf::from(r"C:\ProxSpace\builds")),
        }
    }

    #[test]
    fn writing_creates_etc_on_the_way() {
        let dir = tempfile::tempdir().unwrap();

        assert!(install(dir.path(), &mounts(false), &silent_ui()).unwrap());

        let written = fs::read_to_string(dir.path().join(FSTAB_PATH)).unwrap();
        assert_eq!(written, fixture(BASIC));
    }

    #[test]
    fn writing_the_same_mounts_twice_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &mounts(false), &silent_ui()).unwrap();

        assert!(!install(dir.path(), &mounts(false), &silent_ui()).unwrap());
    }

    #[test]
    fn a_foreign_fstab_is_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(FSTAB_PATH);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, "C:\\elsewhere /setup ntfs noacl 0 0\n").unwrap();

        assert!(install(dir.path(), &mounts(false), &silent_ui()).unwrap());

        let written = fs::read_to_string(&destination).unwrap();
        assert_eq!(written, fixture(BASIC));
        assert!(!written.contains("elsewhere"));
    }

    #[test]
    fn adding_and_removing_the_builds_mount_both_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &mounts(false), &silent_ui()).unwrap();

        assert!(install(dir.path(), &mounts(true), &silent_ui()).unwrap());
        assert!(
            fs::read_to_string(dir.path().join(FSTAB_PATH))
                .unwrap()
                .contains("/builds")
        );

        // The original left the /builds line behind for exactly one run; here
        // it goes away as soon as the command that wanted it is over.
        assert!(install(dir.path(), &mounts(false), &silent_ui()).unwrap());
        assert!(
            !fs::read_to_string(dir.path().join(FSTAB_PATH))
                .unwrap()
                .contains("/builds")
        );
    }
}
