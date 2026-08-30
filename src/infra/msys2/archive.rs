//! Getting the msys2 base archive, and proving it is the right one.
//!
//! The hash is the only thing between a corrupted or substituted download and
//! an install: upstream publishes no checksum file next to the archive, so the
//! constant in [`crate::core::msys2`] is checked here and a file that does not
//! match is deleted rather than kept.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::msys2::ArchiveSource;
use crate::core::paths::Paths;
use crate::core::state::StateError;
use crate::infra::archive::ExtractError;
use crate::infra::download::{self, DownloadError};
use crate::ports::http::HttpClient;
use crate::ui::Ui;
use crate::ui::interrupt;

/// How much of the file is hashed at a time. The archive is tens of megabytes;
/// reading it whole into memory to hash it would be pointless.
const HASH_CHUNK_SIZE: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum Msys2Error {
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("cannot read `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the downloaded archive `{path}` is not the expected file\n  \
         expected sha256 {expected}\n  \
         actual   sha256 {actual}\n  \
         the file has been deleted; run the command again to download it afresh, \
         and if it keeps happening the mirror is serving something else"
    )]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

/// Delete the base archive once it is no longer needed.
///
/// Failing to delete it is not worth failing an otherwise complete install
/// over: the tree is in place either way, so say so and leave the file for the
/// user.
pub fn discard_archive(ui: &Ui, paths: &Paths, source: &ArchiveSource) {
    let archive = source.archive_path(paths);
    if !archive.exists() {
        return;
    }
    match fs::remove_file(&archive) {
        Ok(()) => ui.detail(&format!("removed `{}`", archive.display())),
        Err(error) => ui.warn(&format!(
            "the msys2 tree is in place but `{}` could not be deleted ({error})",
            archive.display()
        )),
    }
}

/// Download the base archive and verify it, reusing whatever is already there.
///
/// A file that is present and correct is left alone — that is what makes a
/// re-run after a failed extraction cheap. A file that is present and wrong is
/// deleted rather than kept: keeping it would mean every later run has to
/// explain the same failure again.
pub fn ensure_archive(
    client: &dyn HttpClient,
    ui: &Ui,
    paths: &Paths,
    source: &ArchiveSource,
) -> Result<PathBuf, Msys2Error> {
    let destination = source.archive_path(paths);

    if destination.is_file() {
        ui.detail(&format!(
            "found `{}`, checking it before downloading again",
            destination.display()
        ));
        match verify(&destination, &source.sha256, ui) {
            Ok(()) => {
                ui.info("the msys2 archive is already downloaded and intact");
                return Ok(destination);
            }
            Err(Msys2Error::Checksum { .. }) => {
                ui.warn("the archive already on disk is damaged; downloading it again");
            }
            Err(other) => return Err(other),
        }
    }

    ui.step(&format!("downloading msys2 {}", source.version));
    ui.detail(&source.url);
    let outcome = download::fetch(client, ui, &source.url, &destination, "msys2 base archive")?;
    ui.detail(&format!(
        "{} bytes{}",
        outcome.bytes,
        if outcome.resumed {
            " (continued an earlier download)"
        } else {
            ""
        }
    ));

    verify(&destination, &source.sha256, ui)?;
    ui.success("msys2 archive downloaded and verified");
    Ok(destination)
}

/// Check a file against an expected sha256, deleting it if it does not match.
pub fn verify(path: &Path, expected: &str, ui: &Ui) -> Result<(), Msys2Error> {
    let actual = sha256_file(path).map_err(|source| Msys2Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if actual.eq_ignore_ascii_case(expected) {
        ui.detail(&format!("sha256 {actual} — as expected"));
        return Ok(());
    }

    // The bytes are worthless, and so is any partial download of them: leaving
    // either behind would make the next run resume onto a bad file.
    let _ = fs::remove_file(path);
    let _ = download::discard(path);
    Err(Msys2Error::Checksum {
        path: path.to_path_buf(),
        expected: expected.to_string(),
        actual,
    })
}

/// sha256 of a file, as lowercase hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK_SIZE];
    loop {
        // Per chunk: the archive is a hundred megabytes, and a Ctrl+C during
        // the hash should stop it rather than be noticed once it is over.
        interrupt::check().map_err(io::Error::other)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use crate::core::msys2::MSYS2_SHA256;

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            std::sync::Arc::new(crate::ui::logging::Logger::disabled()),
        )
    }

    #[test]
    fn hashing_matches_the_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn an_empty_file_hashes_to_the_empty_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        fs::write(&path, b"").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_matching_file_passes_and_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.tar.xz");
        fs::write(&path, b"abc").unwrap();

        verify(
            &path,
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            &silent_ui(),
        )
        .unwrap();

        assert!(path.exists(), "a good archive must not be deleted");
    }

    #[test]
    fn a_mismatching_file_is_deleted_along_with_its_partial_download() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.tar.xz");
        fs::write(&path, b"not the archive").unwrap();
        fs::write(download::part_path(&path), b"leftovers").unwrap();

        let error = verify(&path, &"0".repeat(64), &silent_ui()).unwrap_err();

        assert!(matches!(error, Msys2Error::Checksum { .. }));
        assert!(error.to_string().contains("download it afresh"));
        assert!(!path.exists(), "a bad archive must not be kept");
        assert!(!download::part_path(&path).exists());
    }

    #[test]
    fn a_missing_file_is_an_io_error_not_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let error =
            verify(&dir.path().join("gone.tar.xz"), MSYS2_SHA256, &silent_ui()).unwrap_err();
        assert!(matches!(error, Msys2Error::Io { .. }));
    }
}
