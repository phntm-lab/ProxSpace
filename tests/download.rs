//! Downloading against a local HTTP server.
//!
//! These drive the real `ureq` client rather than a fake one: the behaviour
//! worth testing here — does a Range request actually continue the file, what
//! reaches disk when a connection dies mid-transfer — lives in the interaction
//! between the client and the server, and a fake would only test the fake.
//!
//! The server is a bare `TcpListener` speaking the few lines of HTTP these
//! tests need. A server library turned out to be the wrong tool: the
//! interesting cases here are all misbehaviour — a `Content-Length` that lies
//! about what is sent, a connection cut mid-body — and a correct server
//! implementation exists precisely to make those unrepresentable.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use proxspace::core::msys2;
use proxspace::infra::download::{self, DownloadError};
use proxspace::infra::http::UreqClient;
use proxspace::infra::msys2::archive as msys2_archive;
use proxspace::ports::http::{HttpClient, HttpError, Request};
use proxspace::ui::logging::Logger;
use proxspace::ui::{Ui, UiOptions};

/// Body big enough to take several 64 KiB chunks, so a truncated transfer is a
/// genuinely partial file rather than one short write.
fn payload() -> Vec<u8> {
    (0..300_000u32).map(|index| (index % 251) as u8).collect()
}

fn silent_ui() -> Ui {
    Ui::new(
        UiOptions {
            quiet: true,
            ..UiOptions::default()
        },
        Arc::new(Logger::disabled()),
    )
}

/// How the test server should behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Serve the body, honouring `Range`.
    Correct,
    /// Answer every request with 404.
    NotFound,
    /// On the first request only, announce the full length, send this many
    /// bytes and hang up. Later requests behave correctly.
    DropFirstAfter(usize),
    /// Ignore `Range` entirely and always send the whole body with 200 — what
    /// a mirror behind a transforming proxy does.
    IgnoreRange,
}

struct TestServer {
    url: String,
    address: SocketAddr,
    /// Resume offset seen on each request, in order; `None` means the request
    /// carried no `Range` header.
    ranges: Arc<Mutex<Vec<Option<u64>>>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TestServer {
    fn start(body: Vec<u8>, behaviour: Behaviour) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("cannot bind a test server");
        let address = listener.local_addr().expect("no local address");
        let url = format!("http://{address}/archive.tar.xz");
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));

        let worker = {
            let ranges = Arc::clone(&ranges);
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || {
                let mut served = 0usize;
                for connection in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut connection) = connection else {
                        break;
                    };
                    let Some(offset) = read_request(&mut connection) else {
                        continue;
                    };
                    ranges.lock().unwrap().push(offset);
                    serve(&mut connection, &body, behaviour, offset, served == 0);
                    served += 1;
                    // Every response says `Connection: close`, so ending the
                    // connection here is what the client was told to expect.
                    let _ = connection.shutdown(Shutdown::Both);
                }
            })
        };

        TestServer {
            url,
            address,
            ranges,
            stopping,
            worker: Some(worker),
        }
    }

    fn ranges(&self) -> Vec<Option<u64>> {
        self.ranges.lock().unwrap().clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // The worker is parked in `accept`; one throwaway connection wakes it
        // so it can see the flag and leave.
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Read one request, returning its resume offset: `Some(None)` for a request
/// without a `Range` header, `None` when the peer left without asking anything.
fn read_request(connection: &mut TcpStream) -> Option<Option<u64>> {
    let mut reader = BufReader::new(connection.try_clone().ok()?);
    let mut offset = None;
    let mut line = String::new();
    let mut lines = 0;

    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            // Closed before the blank line: not a request.
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return if lines == 0 { None } else { Some(offset) };
        }
        lines += 1;
        // Header names are case-insensitive and clients differ on which case
        // they send, so match on the lowered line.
        let lowered = trimmed.to_ascii_lowercase();
        if let Some(value) = lowered.strip_prefix("range: bytes=") {
            offset = value.split('-').next().and_then(|start| start.parse().ok());
        }
    }
}

fn head(connection: &mut TcpStream, status: &str, extra: &[String], length: usize) {
    let mut response =
        format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n");
    for header in extra {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    let _ = connection.write_all(response.as_bytes());
}

fn serve(
    connection: &mut TcpStream,
    body: &[u8],
    behaviour: Behaviour,
    offset: Option<u64>,
    first: bool,
) {
    let total = body.len();

    if behaviour == Behaviour::NotFound {
        head(connection, "404 Not Found", &[], 0);
        return;
    }

    if let Behaviour::DropFirstAfter(sent) = behaviour
        && first
    {
        // The length announces the whole file, the body stops early and the
        // connection closes: a mirror dying mid-transfer.
        head(connection, "200 OK", &[], total);
        let _ = connection.write_all(&body[..sent]);
        let _ = connection.flush();
        return;
    }

    let honour_range = behaviour != Behaviour::IgnoreRange;
    match offset.filter(|_| honour_range) {
        Some(offset) if offset as usize >= total => {
            head(
                connection,
                "416 Range Not Satisfiable",
                &[format!("Content-Range: bytes */{total}")],
                0,
            );
        }
        Some(offset) => {
            let rest = &body[offset as usize..];
            head(
                connection,
                "206 Partial Content",
                &[format!(
                    "Content-Range: bytes {offset}-{}/{total}",
                    total - 1
                )],
                rest.len(),
            );
            let _ = connection.write_all(rest);
        }
        None => {
            head(
                connection,
                "200 OK",
                &["Accept-Ranges: bytes".to_string()],
                total,
            );
            let _ = connection.write_all(body);
        }
    }
    let _ = connection.flush();
}

fn fetch(server: &TestServer, destination: &Path) -> Result<download::Outcome, DownloadError> {
    download::fetch(
        &UreqClient::new(),
        &silent_ui(),
        &server.url,
        destination,
        "downloading",
    )
}

/// Guards the harness itself: a test that asserts on a broken transfer is
/// worthless if the server quietly sends a well-formed short body instead.
#[test]
fn the_test_server_really_does_cut_the_body_short() {
    let body = payload();
    let server = TestServer::start(body.clone(), Behaviour::DropFirstAfter(100_000));

    let mut connection = TcpStream::connect(server.address).unwrap();
    connection
        .write_all(b"GET /archive.tar.xz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut raw = Vec::new();
    connection.read_to_end(&mut raw).unwrap();

    let head = String::from_utf8_lossy(&raw[..200.min(raw.len())]).to_string();
    assert!(
        head.contains(&format!("Content-Length: {}", body.len())),
        "the response did not announce the full length: {head}"
    );
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("no end of headers")
        + 4;
    assert_eq!(
        raw.len() - header_end,
        100_000,
        "the body was not truncated"
    );
}

#[test]
fn a_completed_download_is_byte_for_byte_the_file() {
    let body = payload();
    let server = TestServer::start(body.clone(), Behaviour::Correct);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");

    let outcome = fetch(&server, &destination).unwrap();

    assert_eq!(outcome.bytes, body.len() as u64);
    assert!(!outcome.resumed);
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    // Nothing left over: the partial file is gone once it becomes the real one.
    assert!(!download::part_path(&destination).exists());
    assert_eq!(server.ranges(), vec![None]);
}

#[test]
fn the_destination_is_created_along_with_its_directory() {
    let server = TestServer::start(payload(), Behaviour::Correct);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("nested").join("archive.tar.xz");

    fetch(&server, &destination).unwrap();

    assert!(destination.is_file());
}

#[test]
fn a_missing_archive_is_reported_as_a_status_and_writes_nothing() {
    let server = TestServer::start(payload(), Behaviour::NotFound);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");

    let error = fetch(&server, &destination).unwrap_err();

    assert!(
        matches!(
            error,
            DownloadError::Http(HttpError::Status { status: 404, .. })
        ),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("no longer on this mirror"));
    assert!(!destination.exists());
    assert!(!download::part_path(&destination).exists());
}

#[test]
fn a_connection_that_dies_leaves_the_bytes_it_delivered() {
    let body = payload();
    let cut = 100_000;
    let server = TestServer::start(body.clone(), Behaviour::DropFirstAfter(cut));
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");

    let error = fetch(&server, &destination).unwrap_err();

    assert!(
        matches!(
            error,
            DownloadError::Truncated { .. } | DownloadError::Dropped { .. }
        ),
        "unexpected error: {error}"
    );
    // The half-finished file must not be mistakable for the archive...
    assert!(!destination.exists());
    // ...but it has to survive, or there is nothing to resume from.
    let partial = std::fs::read(download::part_path(&destination)).unwrap();
    assert_eq!(partial, body[..cut]);
}

#[test]
fn a_second_run_continues_from_where_the_first_stopped() {
    let body = payload();
    let cut = 100_000;
    let server = TestServer::start(body.clone(), Behaviour::DropFirstAfter(cut));
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");

    fetch(&server, &destination).unwrap_err();
    let outcome = fetch(&server, &destination).unwrap();

    assert!(outcome.resumed, "the second run started over");
    assert_eq!(outcome.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    // The second request asked for exactly the bytes the first one missed.
    assert_eq!(server.ranges(), vec![None, Some(cut as u64)]);
}

/// The real mirror, on demand.
///
/// `MSYS2_URL` answers 302 and sends the client to whichever mirror it picks,
/// on a different host. Whether a `Range` header survives that hop is the
/// client's business, not ours, and getting it wrong is invisible — the
/// download simply starts from zero every time and nobody notices until they
/// lose a connection at 90%. Hence a test that asks the actual mirror.
///
/// ```text
/// cargo test --test download -- --ignored the_real_mirror
/// ```
#[test]
#[ignore = "talks to mirror.msys2.org"]
fn the_real_mirror_honours_a_range_request_across_its_redirect() {
    let client = UreqClient::new();

    let whole = client.send(&Request::get(msys2::MSYS2_URL)).unwrap();
    let total = whole.body_len.expect("the mirror announced no length");
    drop(whole);

    let offset = total - 1_000_000;
    let partial = client
        .send(&Request::resume_from(msys2::MSYS2_URL, offset))
        .unwrap();

    assert!(
        partial.resumed,
        "the mirror answered 200, not 206: the Range header did not survive the redirect"
    );
    assert_eq!(
        partial.body_len,
        Some(1_000_000),
        "206, but for the wrong extent"
    );
}

/// The same thing end to end: a `.part` file holding all but the last megabyte
/// of the real archive, continued over the redirect, and the result checked
/// against the pinned hash. Costs one megabyte of traffic, not fifty.
///
/// ```text
/// PROXSPACE_TEST_ARCHIVE=/path/to/msys2-base-x86_64-20260611.tar.xz \
///     cargo test --test download -- --ignored the_real_archive
/// ```
#[test]
#[ignore = "talks to mirror.msys2.org; set PROXSPACE_TEST_ARCHIVE"]
fn the_real_archive_can_be_finished_from_a_partial_file() {
    let source = std::env::var("PROXSPACE_TEST_ARCHIVE")
        .expect("set PROXSPACE_TEST_ARCHIVE to a downloaded msys2 base archive");
    let complete = std::fs::read(&source).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join(msys2::ArchiveSource::msys2().file_name());
    // Everything but the tail, exactly as an interrupted download would leave it.
    let head = complete.len() - 1_000_000;
    std::fs::write(download::part_path(&destination), &complete[..head]).unwrap();

    let outcome = download::fetch(
        &UreqClient::new(),
        &silent_ui(),
        msys2::MSYS2_URL,
        &destination,
        "downloading",
    )
    .unwrap();

    assert!(outcome.resumed, "the download started over from zero");
    assert_eq!(outcome.bytes, complete.len() as u64);
    // The two halves have to join into the real file, not merely into a file of
    // the right length.
    assert_eq!(
        msys2_archive::sha256_file(&destination).unwrap(),
        msys2::MSYS2_SHA256
    );
}

#[test]
fn a_server_that_ignores_ranges_makes_the_download_start_over() {
    let body = payload();
    let server = TestServer::start(body.clone(), Behaviour::IgnoreRange);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");
    // Leftovers from an earlier attempt that the server will not continue.
    std::fs::write(download::part_path(&destination), vec![0xffu8; 50_000]).unwrap();

    let outcome = fetch(&server, &destination).unwrap();

    assert!(!outcome.resumed);
    assert_eq!(outcome.bytes, body.len() as u64);
    // The stale bytes were dropped rather than prefixed onto the real ones.
    assert_eq!(std::fs::read(&destination).unwrap(), body);
}

#[test]
fn a_partial_file_longer_than_the_archive_is_thrown_away() {
    let body = payload();
    let server = TestServer::start(body.clone(), Behaviour::Correct);
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");
    // Bigger than the file itself: the server can only answer 416.
    std::fs::write(
        download::part_path(&destination),
        vec![0xffu8; body.len() + 1_000],
    )
    .unwrap();

    let outcome = fetch(&server, &destination).unwrap();

    assert_eq!(std::fs::read(&destination).unwrap(), body);
    assert!(!outcome.resumed);
    assert_eq!(
        server.ranges(),
        vec![Some(body.len() as u64 + 1_000), None],
        "the retry should have asked for the whole file"
    );
}

#[test]
fn an_unreachable_server_is_a_transport_error() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("archive.tar.xz");

    let error = download::fetch(
        &UreqClient::new(),
        &silent_ui(),
        // Nothing listens on port 1; the connection is refused at once. A bad
        // hostname would reach the same branch but spend a DNS timeout on it.
        "http://127.0.0.1:1/archive.tar.xz",
        &destination,
        "downloading",
    )
    .unwrap_err();

    assert!(
        matches!(error, DownloadError::Http(HttpError::Transport { .. })),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("cannot reach"));
    assert!(!destination.exists());
}
