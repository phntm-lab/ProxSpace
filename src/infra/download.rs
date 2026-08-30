//! Fetching a file to disk, resumably.
//!
//! The one download this exists for is the msys2 base archive: a hundred and
//! some megabytes over a mirror, which is exactly the size where a dropped
//! connection at 90% is both likely and infuriating. So the bytes go into a
//! sibling `.part` file, and an interrupted transfer is continued with a Range
//! request on the next run instead of started over.
//!
//! Two rules follow from that, and they are the reason this is not a call to
//! `io::copy`:
//!
//! - the destination path only ever exists complete. It appears in one
//!   `rename` after the last byte is on disk, so a half-written file can never
//!   be mistaken for a finished download;
//! - a failure — network, Ctrl+C, disk — leaves the `.part` file behind on
//!   purpose. It is the resume point, not litter; [`discard`] removes it when
//!   the caller decides the bytes are worthless (a hash mismatch, say).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::ports::http::{HttpClient, HttpError, Request};
use crate::ui::Ui;
use crate::ui::interrupt::{self, Interrupted};

/// Read size. Large enough that the syscall overhead disappears against a fast
/// link, small enough that Ctrl+C is noticed promptly — the interrupt flag is
/// checked once per chunk.
const CHUNK_SIZE: usize = 64 * 1024;

/// Suffix of the partial file. Deliberately not the destination name with a
/// dot in front: it has to be obvious in a directory listing what it is.
const PART_SUFFIX: &str = ".part";

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("cannot write `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the download stopped after {received} of {expected} bytes; run the command again to continue it"
    )]
    Truncated { received: u64, expected: u64 },
    #[error(
        "the connection dropped after {received} bytes; run the command again to continue the download"
    )]
    Dropped {
        received: u64,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// What a completed download turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Size of the finished file.
    pub bytes: u64,
    /// True when the transfer continued a `.part` file left by an earlier run.
    pub resumed: bool,
}

/// Where the partial download of `destination` lives.
pub fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(PART_SUFFIX);
    PathBuf::from(name)
}

/// Throw away any partial download of `destination`. Missing file is success —
/// the point is that nothing is left, not that something was removed.
pub fn discard(destination: &Path) -> io::Result<()> {
    match fs::remove_file(part_path(destination)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Download `url` to `destination`, continuing a previous attempt if there is
/// one.
///
/// The destination is replaced only on success. On any failure the partial
/// file stays put so the next call can pick up where this one stopped.
pub fn fetch(
    client: &dyn HttpClient,
    ui: &Ui,
    url: &str,
    destination: &Path,
    message: &str,
) -> Result<Outcome, DownloadError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| DownloadError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    match attempt(client, ui, url, destination, message) {
        // A stale or oversized `.part` makes the server refuse the range. That
        // is not a reason to give up on the download, only on those bytes:
        // drop them and take the file from the top, once.
        Err(DownloadError::Http(HttpError::Status { status: 416, .. })) => {
            ui.detail("the server rejected the resume offset; downloading from the start");
            discard(destination).map_err(|source| DownloadError::Io {
                path: part_path(destination),
                source,
            })?;
            attempt(client, ui, url, destination, message)
        }
        other => other,
    }
}

fn attempt(
    client: &dyn HttpClient,
    ui: &Ui,
    url: &str,
    destination: &Path,
    message: &str,
) -> Result<Outcome, DownloadError> {
    let part = part_path(destination);
    let already_have = existing_len(&part);

    let request = match already_have {
        0 => Request::get(url),
        offset => {
            ui.detail(&format!(
                "continuing an earlier download of `{}` from {offset} bytes",
                part.display()
            ));
            Request::resume_from(url, offset)
        }
    };

    let mut response = client.send(&request)?;

    // A server free to ignore `Range` answers 200 with the whole file. Then the
    // bytes on disk are not a prefix of what is arriving and have to go.
    let resumed = already_have > 0 && response.resumed;
    if already_have > 0 && !resumed {
        ui.detail("the server does not support resuming; downloading from the start");
    }
    let start = if resumed { already_have } else { 0 };
    let expected_total = response.body_len.map(|remaining| remaining + start);

    let mut file = open_part(&part, resumed)?;

    let bar = ui.progress_bytes(expected_total, message);
    bar.set_position(start);

    let mut received = start;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    loop {
        // Between chunks, so Ctrl+C ends the download at a point where what is
        // on disk is exactly what `received` says it is.
        interrupt::check().inspect_err(|_| {
            let _ = file.flush();
            bar.abandon();
        })?;

        let read = match response.body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(source) => {
                let _ = file.flush();
                bar.abandon();
                return Err(DownloadError::Dropped { received, source });
            }
        };
        file.write_all(&buffer[..read])
            .map_err(|source| DownloadError::Io {
                path: part.clone(),
                source,
            })?;
        received += read as u64;
        bar.set_position(received);
    }

    // A connection that closes early looks exactly like a finished one at this
    // level; only the announced length tells them apart.
    if let Some(expected) = expected_total
        && received < expected
    {
        let _ = file.flush();
        bar.abandon();
        return Err(DownloadError::Truncated { received, expected });
    }
    bar.finish_and_clear();

    // Flush and hand the data to the filesystem before the rename, for the same
    // reason the state file does it: a rename that lands before the bytes do
    // would publish an empty archive.
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|source| DownloadError::Io {
            path: part.clone(),
            source,
        })?;
    drop(file);

    fs::rename(&part, destination).map_err(|source| DownloadError::Io {
        path: destination.to_path_buf(),
        source,
    })?;

    Ok(Outcome {
        bytes: received,
        resumed,
    })
}

/// Size of an existing partial download, or 0 if there is nothing usable.
fn existing_len(part: &Path) -> u64 {
    fs::metadata(part).map(|meta| meta.len()).unwrap_or(0)
}

fn open_part(part: &Path, append: bool) -> Result<File, DownloadError> {
    let result = if append {
        OpenOptions::new().append(true).open(part)
    } else {
        // Truncating is the point: whatever was there is not a prefix of what
        // is about to arrive.
        File::create(part)
    };
    result.map_err(|source| DownloadError::Io {
        path: part.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_partial_file_sits_next_to_the_destination() {
        let destination = Path::new(r"C:\ProxSpace\msys2-base-x86_64-20260611.tar.xz");
        assert_eq!(
            part_path(destination),
            PathBuf::from(r"C:\ProxSpace\msys2-base-x86_64-20260611.tar.xz.part")
        );
    }

    #[test]
    fn discarding_a_download_that_never_started_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discard(&dir.path().join("archive.tar.xz")).is_ok());
    }

    #[test]
    fn discarding_removes_only_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("archive.tar.xz");
        fs::write(&destination, b"finished").unwrap();
        fs::write(part_path(&destination), b"half").unwrap();

        discard(&destination).unwrap();

        assert!(!part_path(&destination).exists());
        assert!(destination.exists());
    }

    #[test]
    fn a_missing_partial_file_means_starting_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(existing_len(&dir.path().join("nothing.part")), 0);
    }
}
