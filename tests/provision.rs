//! Getting the msys2 tree in place, and what the state file says when that
//! goes wrong.
//!
//! The archive here is the same mini fixture the extraction tests use, served
//! by a fake [`HttpClient`]: what is being tested is the order of the steps and
//! what survives a failure at each of them, none of which needs a real mirror.
//! The fake also counts its calls, which is how "the second run does nothing"
//! is checked at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proxspace::http::{HttpClient, HttpError, Request, Response};
use proxspace::logging::Logger;
use proxspace::msys2::{self, ArchiveSource, Msys2Error};
use proxspace::paths::Paths;
use proxspace::state::{Stage, State};
use proxspace::ui::{Ui, UiOptions};

const FIXTURE: &str = "tests/fixtures/mini-msys2.tar.xz";

fn silent_ui() -> Ui {
    Ui::new(
        UiOptions {
            quiet: true,
            ..UiOptions::default()
        },
        Arc::new(Logger::disabled()),
    )
}

/// Serves a fixed body, or fails, and remembers how often it was asked.
struct FakeClient {
    body: Vec<u8>,
    failing: bool,
    calls: AtomicUsize,
}

impl FakeClient {
    fn serving(body: Vec<u8>) -> FakeClient {
        FakeClient {
            body,
            failing: false,
            calls: AtomicUsize::new(0),
        }
    }

    fn failing() -> FakeClient {
        FakeClient {
            body: Vec::new(),
            failing: true,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl HttpClient for FakeClient {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failing {
            return Err(HttpError::Transport {
                url: request.url.clone(),
                source: std::io::Error::other("the network is down"),
            });
        }
        // The fake never resumes: a `.part` file is not part of what these
        // tests are about, and the download tests already cover ranges.
        Ok(Response {
            body_len: Some(self.body.len() as u64),
            resumed: false,
            body: Box::new(std::io::Cursor::new(self.body.clone())),
        })
    }
}

/// A source pointing at the fixture, with its real hash so verification passes.
fn fixture_source() -> ArchiveSource {
    ArchiveSource {
        url: "https://mirror.test/distrib/x86_64/msys2-base-x86_64-20260611.tar.xz".to_string(),
        sha256: msys2::sha256_file(Path::new(FIXTURE)).unwrap(),
        version: "20260611".to_string(),
    }
}

fn fixture_bytes() -> Vec<u8> {
    fs::read(FIXTURE).unwrap()
}

struct Sandbox {
    _dir: tempfile::TempDir,
    paths: Paths,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();
        Sandbox { _dir: dir, paths }
    }

    fn tree(&self) -> PathBuf {
        self.paths.msys2()
    }

    fn archive(&self) -> PathBuf {
        fixture_source().archive_path(&self.paths)
    }

    /// The state as it is on disk, not as the caller's copy remembers it.
    fn saved_state(&self) -> State {
        State::load(&self.paths.state_file()).state
    }

    fn provision(&self, client: &dyn HttpClient, state: &mut State) -> Result<(), Msys2Error> {
        msys2::ensure_tree(client, &silent_ui(), &self.paths, state, &fixture_source())
    }
}

#[test]
fn a_fresh_install_downloads_unpacks_and_records_it() {
    let sandbox = Sandbox::new();
    let client = FakeClient::serving(fixture_bytes());
    let mut state = State::default();

    sandbox.provision(&client, &mut state).unwrap();

    assert_eq!(state.stage, Stage::Extracted);
    assert!(sandbox.tree().join("etc/fstab").is_file());
    // The archive is transient: it goes once the tree is in place.
    assert!(!sandbox.archive().exists());

    let saved = sandbox.saved_state();
    assert_eq!(saved.stage, Stage::Extracted);
    let recorded = saved.msys2.expect("no msys2 info recorded");
    assert_eq!(recorded.version, "20260611");
    assert_eq!(recorded.source_url, fixture_source().url);
    assert_eq!(recorded.sha256, fixture_source().sha256);
    assert!(recorded.extracted_at.starts_with("20"), "no timestamp");
    // Where it was installed, so a moved folder can be spotted later.
    assert_eq!(
        saved.install_path.as_deref(),
        Some(sandbox.paths.base().to_string_lossy().as_ref())
    );
}

#[test]
fn a_second_run_touches_neither_the_network_nor_the_tree() {
    let sandbox = Sandbox::new();
    let client = FakeClient::serving(fixture_bytes());
    let mut state = State::default();

    sandbox.provision(&client, &mut state).unwrap();
    let marker = sandbox.tree().join("etc/written-by-the-user");
    fs::write(&marker, b"still here?").unwrap();

    sandbox.provision(&client, &mut state).unwrap();

    assert_eq!(client.calls(), 1, "the archive was downloaded twice");
    assert!(marker.is_file(), "the tree was unpacked over");
    assert_eq!(state.stage, Stage::Extracted);
}

#[test]
fn a_download_that_fails_leaves_the_state_untouched() {
    let sandbox = Sandbox::new();
    let client = FakeClient::failing();
    let mut state = State::default();

    let error = sandbox.provision(&client, &mut state).unwrap_err();

    assert!(
        matches!(error, Msys2Error::Download(_)),
        "unexpected error: {error}"
    );
    assert_eq!(state.stage, Stage::NotInstalled);
    assert!(!sandbox.tree().exists());
    // Nothing claimed to have happened, so there is nothing to write down.
    assert!(!sandbox.paths.state_file().exists());
}

#[test]
fn an_archive_that_fails_its_checksum_never_reaches_the_disk_as_a_tree() {
    let sandbox = Sandbox::new();
    let mut body = fixture_bytes();
    body.extend_from_slice(b"tampered");
    let client = FakeClient::serving(body);
    let mut state = State::default();

    let error = sandbox.provision(&client, &mut state).unwrap_err();

    assert!(
        matches!(error, Msys2Error::Checksum { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(state.stage, Stage::NotInstalled);
    assert!(!sandbox.tree().exists());
    assert!(!sandbox.archive().exists(), "the bad archive was kept");
}

#[test]
fn a_damaged_archive_stops_at_downloaded_and_keeps_nothing_unpacked() {
    let sandbox = Sandbox::new();
    // Passes its checksum, then turns out not to be an archive at all: the
    // failure happens during unpacking, after the download was recorded.
    let body = b"not an archive, but a consistent one".to_vec();
    let source = ArchiveSource {
        url: fixture_source().url,
        sha256: {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("body");
            fs::write(&path, &body).unwrap();
            msys2::sha256_file(&path).unwrap()
        },
        version: "20260611".to_string(),
    };
    let client = FakeClient::serving(body);
    let mut state = State::default();

    let error =
        msys2::ensure_tree(&client, &silent_ui(), &sandbox.paths, &mut state, &source).unwrap_err();

    assert!(
        matches!(error, Msys2Error::Extract(_)),
        "unexpected error: {error}"
    );
    // The download did happen and is recorded as such — but nothing beyond it.
    assert_eq!(state.stage, Stage::Downloaded);
    assert_eq!(sandbox.saved_state().stage, Stage::Downloaded);
    assert!(sandbox.saved_state().msys2.is_none());
    assert!(!sandbox.tree().exists());
    // The archive survives so the next run can retry without downloading.
    assert!(source.archive_path(&sandbox.paths).is_file());
}

#[test]
fn a_tree_left_over_from_an_unfinished_unpack_is_replaced() {
    let sandbox = Sandbox::new();
    let client = FakeClient::serving(fixture_bytes());
    let mut state = State::default();

    // What a run that died between the rename and the state file leaves:
    // a complete-looking tree, an archive still on disk, and a stage that
    // stops at `Downloaded`.
    sandbox.provision(&client, &mut state).unwrap();
    fs::write(sandbox.archive(), fixture_bytes()).unwrap();
    fs::write(sandbox.tree().join("etc/half-finished"), b"junk").unwrap();
    state.move_to(Stage::Downloaded).unwrap();
    state.msys2 = None;
    state.save(&sandbox.paths.state_file()).unwrap();

    sandbox.provision(&client, &mut state).unwrap();

    assert_eq!(state.stage, Stage::Extracted);
    // Unpacked afresh rather than merged into: a tree of unknown provenance is
    // not something to build an install on.
    assert!(!sandbox.tree().join("etc/half-finished").exists());
    assert!(sandbox.tree().join("etc/fstab").is_file());
    assert!(!sandbox.archive().exists());
}

#[test]
fn a_tree_deleted_behind_our_back_is_installed_again() {
    let sandbox = Sandbox::new();
    let client = FakeClient::serving(fixture_bytes());
    let mut state = State::default();

    sandbox.provision(&client, &mut state).unwrap();
    fs::remove_dir_all(sandbox.tree()).unwrap();

    sandbox.provision(&client, &mut state).unwrap();

    assert_eq!(client.calls(), 2, "the archive was not downloaded again");
    assert_eq!(state.stage, Stage::Extracted);
    assert!(sandbox.tree().join("etc/fstab").is_file());
}

#[test]
fn an_archive_left_next_to_a_finished_install_is_cleaned_up() {
    let sandbox = Sandbox::new();
    let client = FakeClient::serving(fixture_bytes());
    let mut state = State::default();

    sandbox.provision(&client, &mut state).unwrap();
    // A run that died after recording the stage but before deleting the file.
    fs::write(sandbox.archive(), fixture_bytes()).unwrap();

    sandbox.provision(&client, &mut state).unwrap();

    assert!(!sandbox.archive().exists());
    assert_eq!(client.calls(), 1);
}
