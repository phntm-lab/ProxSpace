//! Turning an unpacked msys2 tree into a ProxSpace one.
//!
//! The tree here is a bare directory: `prepare()` only ever writes into it, so
//! nothing about a real msys2 install is needed to check what it writes. The
//! one thing that genuinely cannot be faked is `mkpasswd.exe`, which is why the
//! account is handed in — the alternative would be a test that only runs on a
//! machine that already has the thing being set up.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use proxspace::logging::Logger;
use proxspace::msys2::fstab::Mounts;
use proxspace::msys2::{self, PrepareError};
use proxspace::paths::Paths;
use proxspace::ui::{Ui, UiOptions};

/// Real `mkpasswd -c` / `mkgroup -c` output, with the account renamed.
const MKPASSWD: &str = "somebody:unused:1197603:1049089:\
    U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001:/home/somebody:/bin/bash\n";
const MKGROUP: &str = "Domain Users:S-1-5-21-1234567890-987654321-1122334455-513:1049089:\n";

fn silent_ui() -> Ui {
    Ui::new(
        UiOptions {
            quiet: true,
            ..UiOptions::default()
        },
        Arc::new(Logger::disabled()),
    )
}

/// An install directory with an msys2 tree that has been unpacked but not yet
/// touched by ProxSpace.
fn unpacked() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_dir(dir.path()).unwrap();
    fs::create_dir_all(paths.msys2().join("usr/bin")).unwrap();
    (dir, paths)
}

fn prepare(paths: &Paths, mounts: &Mounts) -> Result<msys2::Prepared, PrepareError> {
    msys2::prepare_with_account(paths, &silent_ui(), mounts, MKPASSWD, MKGROUP)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn a_fresh_tree_gets_everything_it_needs() {
    let (_dir, paths) = unpacked();
    let tree = paths.msys2();

    let prepared = prepare(&paths, &Mounts::for_paths(&paths)).unwrap();

    assert!(prepared.changed_anything());

    // The directories msys2 does not create for itself.
    assert!(tree.join("tmp").is_dir());
    assert!(tree.join("dev").is_dir());
    // $HOME, which lives outside the tree and is mounted into it.
    assert!(paths.pm3().is_dir());
    // Created by the original for no reason anyone could trace.
    assert!(!tree.join("otp").exists());
    // Not asked for, so not created.
    assert!(!paths.builds().exists());

    // The assets.
    assert!(
        read(&tree.join("etc/post-install/09-proxspace_setup.post"))
            .contains("PATH=/opt/proxspace/bin:$PATH")
    );
    assert!(read(&tree.join("etc/nsswitch.conf")).contains("passwd: files"));
    assert!(read(&tree.join("opt/proxspace/packages.txt")).contains("mingw-w64-ucrt-x86_64-gcc"));
    for script in ["pm3", "pm3-flash-all", "ps-setup", "ps-info", "proxspace"] {
        let path = tree.join("opt/proxspace/bin").join(script);
        assert!(path.is_file(), "{} was not installed", path.display());
        assert!(read(&path).starts_with("#!/usr/bin/env bash"));
    }

    // The mount table.
    let fstab = read(&tree.join("etc/fstab"));
    assert!(fstab.contains("none / cygdrive"));
    assert!(fstab.contains(&format!("{} /pm3 ntfs noacl 0 0", paths.pm3().display())));
    assert!(!fstab.contains("/setup"));
    assert!(!fstab.contains("/builds"));

    // The account.
    let passwd = read(&tree.join("etc/passwd"));
    assert_eq!(passwd.lines().count(), 1);
    assert!(passwd.starts_with("proxspace:unused:1001:1049089:"));
    assert!(passwd.contains(":/pm3:/bin/bash"));
    assert!(passwd.contains("S-1-5-21-1234567890-987654321-1122334455-1001"));
    assert_eq!(read(&tree.join("etc/group")), MKGROUP);
}

#[test]
fn preparing_a_prepared_tree_changes_nothing() {
    let (_dir, paths) = unpacked();
    let mounts = Mounts::for_paths(&paths);
    prepare(&paths, &mounts).unwrap();

    let before = snapshot(&paths.msys2());
    let again = prepare(&paths, &mounts).unwrap();

    assert!(
        !again.changed_anything(),
        "a second run reported changes: {again:?}"
    );
    assert!(again.directories.is_empty());
    assert_eq!(again.assets.written, Vec::<PathBuf>::new());
    assert!(!again.fstab_changed);
    assert!(!again.userdb.changed);
    assert_eq!(snapshot(&paths.msys2()), before);
}

#[test]
fn everything_it_writes_it_puts_back() {
    let (_dir, paths) = unpacked();
    let tree = paths.msys2();
    let mounts = Mounts::for_paths(&paths);
    prepare(&paths, &mounts).unwrap();
    let expected = snapshot(&tree);

    // A tree that someone has been rummaging around in: an asset edited, the
    // mount table replaced, the account file emptied, a directory deleted.
    fs::write(
        tree.join("etc/post-install/09-proxspace_setup.post"),
        b"broken\n",
    )
    .unwrap();
    fs::write(tree.join("etc/fstab"), b"none / cygdrive binary 0 0\n").unwrap();
    fs::write(tree.join("etc/passwd"), b"").unwrap();
    fs::remove_dir_all(tree.join("tmp")).unwrap();

    let prepared = prepare(&paths, &mounts).unwrap();

    assert!(prepared.changed_anything());
    assert_eq!(snapshot(&tree), expected);
}

#[test]
fn autobuild_gets_its_builds_mount_and_gives_it_back() {
    let (_dir, paths) = unpacked();
    let tree = paths.msys2();

    prepare(&paths, &Mounts::with_builds(&paths)).unwrap();

    assert!(paths.builds().is_dir());
    let fstab = read(&tree.join("etc/fstab"));
    assert!(fstab.contains(&format!(
        "{} /builds ntfs noacl 0 0",
        paths.builds().display()
    )));

    // The next ordinary command drops the mount again. The directory stays:
    // it holds the builds.
    prepare(&paths, &Mounts::for_paths(&paths)).unwrap();
    assert!(!read(&tree.join("etc/fstab")).contains("/builds"));
    assert!(paths.builds().is_dir());
}

#[test]
fn preparing_without_a_tree_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_dir(dir.path()).unwrap();

    let error = prepare(&paths, &Mounts::for_paths(&paths)).unwrap_err();

    assert!(matches!(error, PrepareError::TreeMissing { .. }));
    assert!(error.to_string().contains("has not been unpacked"));
}

#[test]
fn preparing_a_tree_without_the_account_tools_names_the_missing_one() {
    // The full `prepare`, which asks the tree who the user is. An incomplete
    // tree must fail before anything is written rather than halfway through.
    let (_dir, paths) = unpacked();

    let error = msys2::prepare(&paths, &silent_ui(), &Mounts::for_paths(&paths)).unwrap_err();

    assert!(matches!(error, PrepareError::UserDb(_)));
    assert!(error.to_string().contains("mkpasswd.exe"));
    assert!(!paths.msys2().join("etc/fstab").exists());
}

/// Every file under a directory, with its contents: what "nothing changed"
/// actually means.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    collect(root, root, &mut entries);
    entries.sort();
    entries
}

fn collect(root: &Path, dir: &Path, into: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, into);
        } else {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            into.push((relative, fs::read(&path).unwrap()));
        }
    }
}
