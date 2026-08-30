//! Unpacking `.tar.xz` archives.
//!
//! The happy path runs against `tests/fixtures/mini-msys2.tar.xz`, a tiny
//! archive shaped like the msys2 base one (a single `msys64/` root holding a
//! few files and empty directories). It was produced by the system `tar`, not
//! by the crate under test, so a bug in one cannot cancel out a bug in the
//! other. The awkward shapes — two roots, a path escaping the destination — are
//! built here instead: writing them is the only way to get them.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use proxspace::infra::archive::{self, ExtractError};
use proxspace::ui::logging::Logger;
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

fn extract(archive: &Path, destination: &Path) -> Result<archive::Extracted, ExtractError> {
    archive::extract_stripping_root(archive, destination, &silent_ui(), "unpacking")
}

/// Build a `.tar.xz` from `(path, contents)` pairs; `None` contents make a
/// directory entry.
fn write_archive(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
    let file = fs::File::create(path).unwrap();
    let encoder = xz2::write::XzEncoder::new(file, 1);
    let mut builder = tar::Builder::new(encoder);

    for (name, contents) in entries {
        let mut header = tar::Header::new_gnu();
        let bytes = contents.unwrap_or(&[]);
        header.set_size(bytes.len() as u64);
        header.set_mode(if contents.is_some() { 0o644 } else { 0o755 });
        header.set_entry_type(if contents.is_some() {
            tar::EntryType::Regular
        } else {
            tar::EntryType::Directory
        });
        // The name goes into the header by hand rather than through
        // `append_data`, which refuses paths containing `..` — and an archive
        // carrying exactly such a path is one of the things under test.
        let raw = name.as_bytes();
        assert!(raw.len() < 100, "name too long for a ustar header: {name}");
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_cksum();
        builder.append(&header, bytes).unwrap();
    }

    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap().flush().unwrap();
}

#[test]
fn the_root_directory_is_stripped_and_the_contents_land_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");

    let extracted = extract(Path::new(FIXTURE), &destination).unwrap();

    assert_eq!(extracted.root, "msys64");
    // Four directories below the root plus three files; the root itself is not
    // counted because it became `msys2` rather than being written inside it.
    assert_eq!(extracted.entries, 7);

    assert_eq!(
        fs::read_to_string(destination.join("etc/fstab")).unwrap(),
        "hello from the fake msys2\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("usr/bin/hello.sh")).unwrap(),
        "#!/bin/sh\necho hi\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("msys2.ini")).unwrap(),
        "root level file\n"
    );
    // Empty directories have to survive: msys2 expects `tmp/` to exist.
    assert!(destination.join("tmp").is_dir());
    // And nothing keeps the archive's own root name.
    assert!(!destination.join("msys64").exists());
}

#[test]
fn the_staging_directory_is_gone_when_the_tree_is_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");

    extract(Path::new(FIXTURE), &destination).unwrap();

    assert!(!archive::partial_path(&destination).exists());
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "nothing but the unpacked tree should be left behind"
    );
}

#[test]
fn an_existing_destination_is_never_written_into() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("precious.txt"), b"do not touch").unwrap();

    let error = extract(Path::new(FIXTURE), &destination).unwrap_err();

    assert!(matches!(error, ExtractError::DestinationExists(_)));
    assert_eq!(
        fs::read_to_string(destination.join("precious.txt")).unwrap(),
        "do not touch"
    );
}

#[test]
fn leftovers_from_an_interrupted_run_are_cleared_first() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");
    let staging = archive::partial_path(&destination);
    fs::create_dir_all(staging.join("etc")).unwrap();
    // A file from a previous, different attempt that must not survive into the
    // finished tree.
    fs::write(staging.join("etc/stale.conf"), b"from an older run").unwrap();

    extract(Path::new(FIXTURE), &destination).unwrap();

    assert!(!destination.join("etc/stale.conf").exists());
    assert!(destination.join("etc/fstab").is_file());
}

#[test]
fn a_damaged_archive_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");
    let broken = dir.path().join("broken.tar.xz");
    // Big enough that the truncation lands in the middle of the entries, so
    // the failure happens with files already written into the staging
    // directory — which is the case worth checking.
    let whole = dir.path().join("whole.tar.xz");
    let names: Vec<String> = (0..200)
        .map(|index| format!("msys64/file{index}"))
        .collect();
    let entries: Vec<(&str, Option<&[u8]>)> = names
        .iter()
        .map(|name| (name.as_str(), Some(b"some contents here".as_slice())))
        .collect();
    write_archive(&whole, &entries);
    let mut bytes = fs::read(&whole).unwrap();
    bytes.truncate(bytes.len() / 2);
    fs::write(&broken, &bytes).unwrap();

    let error = extract(&broken, &destination).unwrap_err();

    assert!(
        matches!(error, ExtractError::Corrupt { .. }),
        "unexpected error: {error}"
    );
    assert!(!destination.exists(), "a failed unpack must not be visible");
    assert!(!archive::partial_path(&destination).exists());
}

#[test]
fn an_archive_that_is_not_xz_at_all_is_reported_as_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("nonsense.tar.xz");
    fs::write(&broken, b"this is not an archive").unwrap();

    let error = extract(&broken, &dir.path().join("msys2")).unwrap_err();

    assert!(matches!(error, ExtractError::Corrupt { .. }));
    assert!(error.to_string().contains("download it afresh"));
}

#[test]
fn a_missing_archive_is_reported_as_such() {
    let dir = tempfile::tempdir().unwrap();
    let error = extract(&dir.path().join("gone.tar.xz"), &dir.path().join("msys2")).unwrap_err();
    assert!(matches!(error, ExtractError::Open { .. }));
}

#[test]
fn two_top_level_directories_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("two-roots.tar.xz");
    write_archive(
        &archive_path,
        &[
            ("msys64/etc/fstab", Some(b"one".as_slice())),
            ("something-else/readme", Some(b"two".as_slice())),
        ],
    );

    let error = extract(&archive_path, &dir.path().join("msys2")).unwrap_err();

    assert!(
        matches!(error, ExtractError::MixedRoots { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn an_entry_pointing_outside_the_destination_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("escape.tar.xz");
    write_archive(
        &archive_path,
        &[
            ("msys64/etc/fstab", Some(b"fine".as_slice())),
            ("msys64/../../escaped.txt", Some(b"owned".as_slice())),
        ],
    );
    let destination = dir.path().join("msys2");

    let error = extract(&archive_path, &destination).unwrap_err();

    assert!(
        matches!(error, ExtractError::UnsafePath(_)),
        "unexpected error: {error}"
    );
    assert!(!destination.exists());
    assert!(!archive::partial_path(&destination).exists());
    // The whole point: nothing was written next to the destination either.
    let escaped: Vec<PathBuf> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().unwrap() != "escape.tar.xz")
        .collect();
    assert!(escaped.is_empty(), "these were written: {escaped:?}");
}

/// The real thing, on demand: the fixture proves the shape is handled, this
/// proves the actual archive is. Not part of a normal run — it wants a
/// downloaded copy of the base archive and a few hundred megabytes of disk.
///
/// ```text
/// PROXSPACE_TEST_ARCHIVE=/path/to/msys2-base-x86_64-20260611.tar.xz \
///     cargo test --test archive -- --ignored
/// ```
#[test]
#[ignore = "needs a downloaded msys2 base archive; set PROXSPACE_TEST_ARCHIVE"]
fn the_real_base_archive_unpacks() {
    let Ok(source) = std::env::var("PROXSPACE_TEST_ARCHIVE") else {
        panic!("set PROXSPACE_TEST_ARCHIVE to a downloaded msys2 base archive");
    };
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("msys2");

    let extracted = extract(Path::new(&source), &destination).unwrap();

    assert_eq!(extracted.root, "msys64");
    assert!(
        extracted.entries > 10_000,
        "only {} entries",
        extracted.entries
    );
    // The three things every later stage depends on being where we put them.
    assert!(destination.join("usr/bin/bash.exe").is_file());
    assert!(destination.join("usr/bin/pacman.exe").is_file());
    assert!(destination.join("etc").is_dir());
}

#[test]
fn an_empty_archive_is_refused_rather_than_producing_an_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("empty.tar.xz");
    write_archive(&archive_path, &[]);
    let destination = dir.path().join("msys2");

    let error = extract(&archive_path, &destination).unwrap_err();

    assert!(matches!(error, ExtractError::Empty));
    assert!(!destination.exists());
}
