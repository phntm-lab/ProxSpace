//! The msys2 tree: which base archive it comes from, and getting that archive
//! onto disk intact.
//!
//! Everything version-specific about msys2 lives in the constants below, in one
//! place, because bumping msys2 means editing exactly them.

pub mod fstab;
pub mod procs;
pub mod shell;
pub mod userdb;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::assets::{self, AssetError};
use crate::core::paths::Paths;
use crate::core::state::{Msys2Info, Stage, State, StateError, timestamp};
use crate::infra::archive::{self, ExtractError};
use crate::infra::download::{self, DownloadError};
use crate::infra::msys2::fstab::FstabError;
use crate::infra::msys2::procs::ProcsError;
use crate::infra::msys2::userdb::UserDbError;
use crate::ports::command::{Cmd, CommandError, CommandRunner};
use crate::ports::http::HttpClient;
use crate::ui::Ui;
use crate::ui::interrupt::{self, Interrupted};

/// Datestamp of the base archive this build installs, in the `YYYYMMDD` form
/// upstream names its archives with.
///
/// This is the version an installed tree is compared against, so bumping msys2
/// is a matter of editing this block — and only this block — as a unit:
///
/// 1. Pick the newest `msys2-base-x86_64-<datestamp>.tar.xz` from
///    <https://repo.msys2.org/distrib/x86_64/> and put its datestamp here.
/// 2. Point [`MSYS2_URL`] at that file.
/// 3. Download it, hash it, and put the hash in [`MSYS2_SHA256`], following the
///    note there about cross-checking a second mirror.
/// 4. Leave [`MSYS2_MIN_COMPATIBLE`] where it is unless the new runtime cannot
///    be reached from the old ones by `pacman -Syuu`.
///
/// Versions are ordered by comparing these strings, which works only while the
/// datestamp form holds; the tests below check the constants against each other
/// and against that form.
pub const MSYS2_VERSION: &str = "20260611";

/// Where the archive for [`MSYS2_VERSION`] is downloaded from. The file name is
/// parsed back out of this URL, so it has to keep the upstream name.
pub const MSYS2_URL: &str =
    "https://mirror.msys2.org/distrib/x86_64/msys2-base-x86_64-20260611.tar.xz";

/// sha256 of the archive at [`MSYS2_URL`].
///
/// Computed from the file itself and cross-checked against a second mirror.
/// Upstream publishes no checksum file alongside the archive, so this constant
/// is the only thing standing between a corrupted or substituted download and
/// an install; it must be recomputed whenever [`MSYS2_URL`] changes.
pub const MSYS2_SHA256: &str = "a2d047e8ee213c3c6a49a8de427eb1069df12207c0422ff1b3cbb5c905c34221";

/// Oldest msys2 version that `pacman -Syuu` can still bring fully up to date.
/// Bump ONLY when upstream breaks the upgrade path from older runtimes:
/// users below this version get a full reinstall instead of an in-place upgrade.
pub const MSYS2_MIN_COMPATIBLE: &str = "20260611";

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

/// Which base archive to install: the constants above, in a form that can be
/// pointed elsewhere.
///
/// The indirection exists for the tests. Provisioning is the part of this
/// module worth testing — what the state file says after a failure halfway
/// through, what a second run does — and a function wired directly to
/// [`MSYS2_URL`] could only be tested by downloading fifty megabytes and
/// hoping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSource {
    pub url: String,
    pub sha256: String,
    pub version: String,
}

impl ArchiveSource {
    /// The archive this build of ProxSpace installs.
    pub fn msys2() -> ArchiveSource {
        ArchiveSource {
            url: MSYS2_URL.to_string(),
            sha256: MSYS2_SHA256.to_string(),
            version: MSYS2_VERSION.to_string(),
        }
    }

    /// Name the archive is saved under, taken from the URL so the two cannot
    /// drift apart.
    pub fn file_name(&self) -> &str {
        file_name_of(&self.url)
    }

    /// Where it is downloaded to: next to the binary, like everything else. It
    /// exists only between the download and the unpacking.
    pub fn archive_path(&self, paths: &Paths) -> PathBuf {
        paths.base().join(self.file_name())
    }
}

/// Get the msys2 tree in place, resuming whatever a previous run left behind.
///
/// The order of the last three steps is the whole point of this function:
/// unpack, *then* record the stage, *then* delete the archive. Every way a run
/// can die in the middle leaves a state that the next run reads correctly:
///
/// - died during the download — the stage is at most `Downloaded`, and the
///   partial file is continued;
/// - died during the unpacking — the tree is still a `.partial` directory and
///   the stage is `Downloaded`, so the archive is still there to unpack again;
/// - died between the rename and the state file — the stage still says
///   `Downloaded`, so the finished tree is thrown away and unpacked again from
///   the archive that has not been deleted yet. Redundant work, but never a
///   tree of unknown provenance;
/// - died between the state file and deleting the archive — the stage says
///   `Extracted` and the leftover archive is cleaned up on the next run.
///
/// What it deliberately does not do is decide whether an *existing* tree should
/// be replaced because a newer msys2 shipped: that decision needs the whole
/// picture and belongs with the rest of the install pipeline.
pub fn ensure_tree(
    client: &dyn HttpClient,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    source: &ArchiveSource,
) -> Result<(), Msys2Error> {
    let tree = paths.msys2();
    let state_file = paths.state_file();

    if state.stage >= Stage::Extracted {
        if tree.is_dir() {
            // The one case where an archive may still be lying around next to a
            // finished install: the previous run died before deleting it.
            discard_archive(ui, paths, source);
            return Ok(());
        }
        ui.warn(&format!(
            "the state file says msys2 is installed, but `{}` is not there; installing it again",
            tree.display()
        ));
        // Whatever the state recorded described the tree that is gone: the
        // packages in it and the python extras added to it went with it.
        state.forget_msys2()?;
        state.save(&state_file)?;
    }

    let archive = ensure_archive(client, ui, paths, source)?;
    state.move_to(Stage::Downloaded)?;
    state.save(&state_file)?;

    // Anything at the destination now is the wreckage of an unpacking that
    // never finished — the state file would say `Extracted` otherwise.
    if tree.exists() {
        ui.detail(&format!(
            "removing an unfinished msys2 tree in `{}`",
            tree.display()
        ));
        archive::remove_tree(&tree)?;
    }

    ui.step("unpacking msys2");
    let extracted = archive::extract_stripping_root(&archive, &tree, ui, "unpacking")?;
    ui.detail(&format!(
        "{} entries from `{}/`",
        extracted.entries, extracted.root
    ));

    state.msys2 = Some(Msys2Info {
        version: source.version.clone(),
        source_url: source.url.clone(),
        sha256: source.sha256.clone(),
        extracted_at: timestamp(),
    });
    state.install_path = Some(paths.base().to_string_lossy().into_owned());
    state.move_to(Stage::Extracted)?;
    state.save(&state_file)?;

    discard_archive(ui, paths, source);
    ui.success(&format!("msys2 unpacked into `{}`", tree.display()));
    Ok(())
}

/// Delete the base archive once it is no longer needed.
///
/// Failing to delete it is not worth failing an otherwise complete install
/// over: the tree is in place either way, so say so and leave the file for the
/// user.
fn discard_archive(ui: &Ui, paths: &Paths, source: &ArchiveSource) {
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

/// Last path segment of a URL, without any query string.
fn file_name_of(url: &str) -> &str {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url)
}

/// Datestamp out of an archive name such as
/// `msys2-base-x86_64-20260611.tar.xz`. Used to tell which base version a tree
/// came from when the state file does not say.
pub fn version_from_url(url: &str) -> Option<&str> {
    let name = file_name_of(url);
    let version = name
        .strip_prefix("msys2-base-x86_64-")?
        .strip_suffix(".tar.xz")?;
    let is_datestamp = version.len() == 8 && version.bytes().all(|byte| byte.is_ascii_digit());
    is_datestamp.then_some(version)
}

/// The msys2 subsystem ProxSpace runs in. It decides which prefix
/// (`/ucrt64`) is on `$PATH` and which package set the environment is built
/// from; the original used `MINGW64`, this port moved to UCRT64.
pub const MSYSTEM: &str = "UCRT64";

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

/// Directories msys2 needs but does not create for itself. The base archive
/// ships without them and `/etc/profile` assumes both are there.
///
/// `otp/`, which `setup.cmd` also created, is deliberately not here: nothing in
/// any script of the original ever refers to it.
const REQUIRED_DIRS: &[&str] = &["tmp", "dev"];

#[derive(Debug, Error)]
pub enum PrepareError {
    #[error("`{path}` is not there; msys2 has not been unpacked yet")]
    TreeMissing { path: PathBuf },
    #[error("cannot create `{path}`")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Assets(#[from] AssetError),
    #[error(transparent)]
    Fstab(#[from] FstabError),
    #[error(transparent)]
    UserDb(#[from] UserDbError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// Everything [`prepare`] found or changed.
#[derive(Debug)]
pub struct Prepared {
    pub directories: Vec<PathBuf>,
    pub assets: assets::Report,
    pub fstab_changed: bool,
    pub userdb: userdb::Written,
}

impl Prepared {
    /// Whether the tree was actually touched. A quiet `false` on every run
    /// after the first is the point of the whole function.
    pub fn changed_anything(&self) -> bool {
        !self.directories.is_empty()
            || self.assets.changed_anything()
            || self.fstab_changed
            || self.userdb.changed
    }
}

/// Bring an unpacked msys2 tree to the state ProxSpace expects.
///
/// This is `setup/setup.cmd`, minus the parts that turned out to be either
/// pointless or actively unwanted: no `otp/`, no deleting `/etc/passwd` only to
/// write the same thing back, and no `rebaseall` — that one now happens only
/// when asked for, because it took a minute off every single start to fix a
/// problem most installs never have.
///
/// Cheap and idempotent by design, so that every command can call it first
/// without anyone having to reason about whether this particular one needs to.
/// The way it stays cheap is that each step compares before it writes.
pub fn prepare(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    mounts: &fstab::Mounts,
) -> Result<Prepared, PrepareError> {
    let tree = paths.msys2();
    if !tree.is_dir() {
        return Err(PrepareError::TreeMissing { path: tree });
    }

    // Asked before anything is written, because the answer comes from two
    // external programs and failing after a half-written tree would be worse.
    let account = userdb::query(runner, ui, &tree)?;
    prepare_with_account(ui, paths, mounts, &account.passwd, &account.group)
}

/// The same, for an account already looked up.
///
/// Exists so the whole of the provisioning can be exercised on a directory that
/// only looks like an msys2 tree: `mkpasswd.exe` cannot be faked, and a step
/// this central should not be testable only on a machine with a real install.
pub fn prepare_with_account(
    ui: &Ui,
    paths: &Paths,
    mounts: &fstab::Mounts,
    mkpasswd_output: &str,
    mkgroup_output: &str,
) -> Result<Prepared, PrepareError> {
    let tree = paths.msys2();
    if !tree.is_dir() {
        return Err(PrepareError::TreeMissing { path: tree });
    }
    interrupt::check()?;

    let mut directories = Vec::new();
    for name in REQUIRED_DIRS {
        directories.extend(ensure_dir(&tree.join(name))?);
    }
    // $HOME lives outside the tree and is mounted into it; without it the
    // mount resolves to nothing and the first login lands in `/`.
    directories.extend(ensure_dir(&mounts.pm3)?);
    if let Some(builds) = &mounts.builds {
        directories.extend(ensure_dir(builds)?);
    }
    for path in &directories {
        ui.detail(&format!("created `{}`", path.display()));
    }

    let assets = assets::install(&tree, ui)?;
    let fstab_changed = fstab::install(&tree, mounts, ui)?;
    let userdb = userdb::install_from(&tree, mkpasswd_output, mkgroup_output, ui)?;

    Ok(Prepared {
        directories,
        assets,
        fstab_changed,
        userdb,
    })
}

/// Create a directory, reporting it only if it was not already there.
fn ensure_dir(path: &Path) -> Result<Option<PathBuf>, PrepareError> {
    if path.is_dir() {
        return Ok(None);
    }
    fs::create_dir_all(path).map_err(|source| PrepareError::CreateDir {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(path.to_path_buf()))
}

#[derive(Debug, Error)]
pub enum RebaseError {
    #[error("`{path}` is missing; the msys2 tree is incomplete")]
    ToolMissing { path: PathBuf },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Procs(#[from] ProcsError),
}

/// Recompute the load addresses of the msys2 DLLs (`rebaseall`).
///
/// Cygwin's `fork()` needs every DLL to land at the same address in the child
/// as in the parent, and it cannot when two of them want the same range. The
/// symptom is builds failing at random with "unable to remap" — and nothing
/// else looks wrong, which is why the original ran this on every single start.
///
/// Here it runs only for `repair --rebase`. It is slow, it requires that
/// nothing else in the tree is running, and it fixes a problem that most
/// installs never have; paying for it on every start to avoid explaining it
/// once was the wrong trade.
pub fn rebase(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Result<(), RebaseError> {
    let tree = paths.msys2();
    let dash = tree.join("usr/bin/dash.exe");
    if !dash.is_file() {
        return Err(RebaseError::ToolMissing { path: dash });
    }

    // rebaseall rewrites the DLLs in place and refuses, or corrupts them, if
    // anything is using them.
    procs::stop_holders(&tree, ui)?;

    ui.step("rebasing the msys2 DLLs (this takes a while)");
    let spinner = ui.spinner("rebasing");
    // The command is long and chatty and its output — a list of every DLL it
    // touched — matters only when it goes wrong, so it stays in the log unless
    // `--verbose` asks for it.
    let output = runner.run(
        ui,
        &Cmd::new(&dash)
            .arg("/usr/bin/rebaseall")
            .arg("-p")
            .label("rebasing the msys2 DLLs")
            .quiet(),
    );
    spinner.finish_and_clear();
    output?.check()?;

    ui.success("msys2 DLLs rebased");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_constants_agree_with_each_other() {
        assert_eq!(version_from_url(MSYS2_URL), Some(MSYS2_VERSION));
        assert_eq!(
            ArchiveSource::msys2().file_name(),
            format!("msys2-base-x86_64-{MSYS2_VERSION}.tar.xz")
        );
        for version in [MSYS2_VERSION, MSYS2_MIN_COMPATIBLE] {
            assert_eq!(
                version.len(),
                8,
                "versions are compared as strings, so they must all be datestamps"
            );
            assert!(version.bytes().all(|byte| byte.is_ascii_digit()));
        }
        assert_eq!(MSYS2_SHA256.len(), 64);
        assert!(MSYS2_SHA256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            MSYS2_MIN_COMPATIBLE <= MSYS2_VERSION,
            "the oldest supported version cannot be newer than the shipped one"
        );
    }

    #[test]
    fn the_file_name_comes_from_the_url() {
        assert_eq!(
            file_name_of("https://example.test/a/b/msys2-base-x86_64-20260611.tar.xz"),
            "msys2-base-x86_64-20260611.tar.xz"
        );
        assert_eq!(
            file_name_of("https://example.test/get?file=msys2-base-x86_64-20260611.tar.xz"),
            "get"
        );
    }

    #[test]
    fn the_version_is_read_out_of_the_archive_name() {
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-20270115.tar.xz"),
            Some("20270115")
        );
        // Anything that is not the expected shape is not guessed at.
        assert_eq!(
            version_from_url("https://example.test/msys2-base-i686-20260611.tar.xz"),
            None
        );
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-latest.tar.xz"),
            None
        );
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-20260611.tar.gz"),
            None
        );
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
