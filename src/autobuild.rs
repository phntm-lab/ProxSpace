//! `autobuild`: build every proxmark3 checkout in `pm3/` and pack each result.
//!
//! This is `autobuild.bat` of the original, which killed `gpg-agent`, appended
//! a `/builds` line to `fstab` and ran `setup/autobuild.sh` in a login shell.
//! The build itself is still that script — it follows whatever the makefiles of
//! a checkout need, and turning that into Rust would mean maintaining a second
//! opinion about how proxmark3 is built. What moved here is everything the
//! script should not be doing itself: mounting the output directory, giving the
//! mount back afterwards, and installing the one package it depends on.
//!
//! The `/builds` mount is put in place for this command and taken away when it
//! ends, which is what the original got by rewriting `fstab` on every start.
//! The directory stays: it holds the archives.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::assets::AUTOBUILD_PATH;
use crate::command::CommandRunner;
use crate::interrupt::{self, Interrupted};
use crate::msys2::fstab::{self, FstabError, Mounts};
use crate::msys2::shell::{self, ShellError};
use crate::pacman::{Mode, Pacman, PacmanError};
use crate::paths::Paths;
use crate::ui::Ui;

/// Where `7z` comes from. Installed on demand rather than shipped in the
/// package set, as the original did: it is of no use to anyone who never runs
/// this command, and it is a single small package when they do.
pub const ARCHIVER_PACKAGE: &str = "p7zip";

#[derive(Debug, Error)]
pub enum AutobuildError {
    #[error("`{path}` is missing; the msys2 tree is incomplete")]
    ScriptMissing { path: PathBuf },
    #[error("cannot read `{path}`")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot create `{path}`")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Pacman(#[from] PacmanError),
    #[error(transparent)]
    Fstab(#[from] FstabError),
    #[error(transparent)]
    Shell(#[from] ShellError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// Build everything in `pm3/`, returning the exit code of the build script.
///
/// The environment is expected to be installed already; the caller brings it up
/// exactly as it does for `shell`.
pub fn run(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Result<i32, AutobuildError> {
    interrupt::check()?;

    let tree = paths.msys2();
    let script = tree.join(AUTOBUILD_PATH);
    if !script.is_file() {
        return Err(AutobuildError::ScriptMissing { path: script });
    }

    // Asked first, because the answer is usually "nothing to do" on a fresh
    // install and every step below it costs the user something.
    let checkouts = checkouts(&paths.pm3())?;
    if checkouts.is_empty() {
        ui.warn(&format!(
            "there is nothing to build: `{}` holds no proxmark3 checkout",
            paths.pm3().display()
        ));
        ui.info("clone one into it, for example `git clone https://github.com/RfidResearchGroup/proxmark3.git`");
        return Ok(0);
    }
    ui.info(&format!("checkouts to build: {}", checkouts.join(", ")));

    ensure_archiver(runner, ui, &tree)?;

    mount_builds(ui, paths)?;
    let result = shell::exec(paths, &script_args(paths, ui));
    // Whatever happened, the mount goes back the way it was. A failure to put
    // it back is not worth losing the build's own outcome over: the next
    // command through `prepare` rewrites `fstab` anyway.
    if let Err(error) = unmount_builds(ui, paths) {
        ui.warn(&format!("cannot restore `/etc/fstab`: {error}"));
    }

    let code = result?;
    if code == 0 {
        ui.success(&format!("builds are in `{}`", paths.builds().display()));
    } else {
        ui.error(&format!("the build script exited with {code}"));
    }
    Ok(code)
}

/// The script's path as the shell sees it.
fn script_command() -> String {
    format!("/{AUTOBUILD_PATH}")
}

/// What the shell is asked to run: the script, and where to put the archives.
///
/// The output directory is named rather than left to the `/builds` mount. A
/// mount added moments ago is not necessarily visible: cygwin builds the mount
/// table once per installation, and every process started while another one is
/// still alive — an open shell, a `gpg-agent` left behind by pacman — reuses
/// the table that was current when the first of them started. The mount is
/// still written, because it is the name a person types; nothing depends on it.
fn script_args(paths: &Paths, ui: &Ui) -> Vec<String> {
    let mut args = vec![script_command()];
    match posix_path(&paths.builds()) {
        Some(builds) => args.push(builds),
        // Only a path that is not on a drive letter gets here, and the install
        // path check has already refused most of those. The mount is then the
        // only way in, so say what it depends on rather than fail outright.
        None => ui.warn(&format!(
            "`{}` cannot be named inside the shell; the build will use the \
             /builds mount, which needs every other ProxSpace window closed",
            paths.builds().display()
        )),
    }
    args
}

/// A Windows path as the shell sees it: `D:\ProxSpace\builds` becomes
/// `/d/ProxSpace/builds`.
///
/// The mount table drops the `/cygdrive` prefix (`fstab.rs`), so every drive is
/// a single letter at the root. Anything that is not a drive-letter path — a
/// UNC share, most of all — has no such name and gives `None`.
fn posix_path(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    let (drive, rest) = text.split_once(":/")?;
    let mut letters = drive.chars();
    let letter = letters.next().filter(char::is_ascii_alphabetic)?;
    if letters.next().is_some() {
        return None;
    }
    Some(format!(
        "/{}/{}",
        letter.to_ascii_lowercase(),
        rest.trim_end_matches('/')
    ))
}

/// Every subdirectory of `pm3/`, which is what the script iterates over.
///
/// Dotted names are skipped: `.git` and the like are not checkouts, and the
/// script's own `ls -d */` would not list them either.
pub fn checkouts(pm3: &Path) -> Result<Vec<String>, AutobuildError> {
    if !pm3.is_dir() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    let entries = fs::read_dir(pm3).map_err(|source| AutobuildError::ReadDir {
        path: pm3.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AutobuildError::ReadDir {
            path: pm3.to_path_buf(),
            source,
        })?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Make sure `7z` is there, installing it if it is not.
pub fn ensure_archiver(
    runner: &dyn CommandRunner,
    ui: &Ui,
    tree: &Path,
) -> Result<(), AutobuildError> {
    let pacman = Pacman::new(tree);
    if pacman
        .query_installed(runner, ui)?
        .contains(ARCHIVER_PACKAGE)
    {
        ui.detail(&format!("`{ARCHIVER_PACKAGE}` is installed"));
        return Ok(());
    }

    ui.step(&format!(
        "installing `{ARCHIVER_PACKAGE}`, which packs the archives"
    ));
    pacman.install(runner, ui, &[ARCHIVER_PACKAGE], Mode::Needed)?;
    Ok(())
}

/// Add `/builds` to the mount table, creating the directory it points at.
pub fn mount_builds(ui: &Ui, paths: &Paths) -> Result<(), AutobuildError> {
    let builds = paths.builds();
    if !builds.is_dir() {
        fs::create_dir_all(&builds).map_err(|source| AutobuildError::CreateDir {
            path: builds.clone(),
            source,
        })?;
        ui.detail(&format!("created `{}`", builds.display()));
    }

    fstab::install(&paths.msys2(), &Mounts::with_builds(paths), ui)?;
    ui.detail(&format!("`{}` is mounted as /builds", builds.display()));
    Ok(())
}

/// Take it away again.
pub fn unmount_builds(ui: &Ui, paths: &Paths) -> Result<(), AutobuildError> {
    fstab::install(&paths.msys2(), &Mounts::for_paths(paths), ui)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::command::{Cmd, CommandError, Output};
    use crate::msys2::fstab::FSTAB_PATH;

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::logging::Logger::disabled()),
        )
    }

    /// A pacman that answers `-Q` with whatever it was given and records every
    /// command line it was asked to run.
    struct Fake {
        installed: String,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn with(installed: &str) -> Fake {
            Fake {
                installed: installed.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for Fake {
        fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
            let line = cmd.command_line();
            self.calls.lock().unwrap().push(line.clone());
            let stdout = if line.contains(" -Q ") || line.ends_with(" -Q") {
                self.installed.clone()
            } else {
                String::new()
            };
            Ok(Output::new(Some(0), &stdout, "", cmd.describe()))
        }
    }

    /// A base directory with just enough of a tree for pacman to be callable.
    fn environment() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();
        fs::create_dir_all(paths.msys2().join("usr/bin")).unwrap();
        fs::write(paths.msys2().join("usr/bin/pacman.exe"), b"pacman").unwrap();
        (dir, paths)
    }

    #[test]
    fn the_script_is_run_from_where_it_is_installed() {
        assert_eq!(script_command(), "/opt/proxspace/autobuild.sh");
        assert!(
            crate::assets::assets()
                .iter()
                .any(|asset| asset.destination(Path::new("root"))
                    == Path::new("root").join(AUTOBUILD_PATH)),
            "the script is not among the assets"
        );
    }

    #[test]
    fn the_output_directory_is_named_the_way_the_shell_sees_it() {
        assert_eq!(
            posix_path(Path::new(r"D:\ProxSpaceTest\builds")).as_deref(),
            Some("/d/ProxSpaceTest/builds")
        );
        assert_eq!(
            posix_path(Path::new(r"C:\ProxSpace\builds\")).as_deref(),
            Some("/c/ProxSpace/builds")
        );
        // Not a drive: there is no name for it with the cygdrive prefix gone.
        assert_eq!(posix_path(Path::new(r"\\server\share\builds")), None);
        assert_eq!(posix_path(Path::new("/already/posix")), None);
    }

    /// The mount cannot be relied on — cygwin fixes its mount table when the
    /// first process of an installation starts — so the script is told where
    /// the archives go.
    #[test]
    fn the_script_is_told_where_to_put_the_archives() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();

        let args = script_args(&paths, &silent_ui());

        assert_eq!(args.len(), 2, "got: {args:?}");
        assert_eq!(args[0], "/opt/proxspace/autobuild.sh");
        assert_eq!(args[1], posix_path(&paths.builds()).unwrap());
        assert!(args[1].ends_with("/builds"), "got: {}", args[1]);
    }

    #[test]
    fn only_directories_count_as_checkouts() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("proxmark3")).unwrap();
        fs::create_dir(dir.path().join("iceman")).unwrap();
        fs::create_dir(dir.path().join(".cache")).unwrap();
        fs::write(dir.path().join("notes.txt"), b"").unwrap();

        assert_eq!(checkouts(dir.path()).unwrap(), ["iceman", "proxmark3"]);
    }

    #[test]
    fn a_missing_home_directory_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(checkouts(&dir.path().join("pm3")).unwrap().is_empty());
    }

    #[test]
    fn an_installed_archiver_is_left_alone() {
        let (_dir, paths) = environment();
        let fake = Fake::with("p7zip 17.05-1\nmake 4.4-1\n");

        ensure_archiver(&fake, &silent_ui(), &paths.msys2()).unwrap();

        assert_eq!(fake.calls().len(), 1, "got: {:?}", fake.calls());
    }

    #[test]
    fn a_missing_archiver_is_installed() {
        let (_dir, paths) = environment();
        let fake = Fake::with("make 4.4-1\n");

        ensure_archiver(&fake, &silent_ui(), &paths.msys2()).unwrap();

        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("-S") && call.contains(ARCHIVER_PACKAGE)),
            "got: {calls:?}"
        );
    }

    #[test]
    fn the_mount_is_added_for_the_command_and_given_back_after_it() {
        let (_dir, paths) = environment();
        let fstab = paths.msys2().join(FSTAB_PATH);
        let ui = silent_ui();

        mount_builds(&ui, &paths).unwrap();

        assert!(paths.builds().is_dir());
        assert!(fs::read_to_string(&fstab).unwrap().contains("/builds"));

        unmount_builds(&ui, &paths).unwrap();

        assert!(!fs::read_to_string(&fstab).unwrap().contains("/builds"));
        // The archives live there; only the mount is temporary.
        assert!(paths.builds().is_dir());
    }

    #[test]
    fn a_tree_without_the_script_says_so_rather_than_starting_a_shell() {
        let (_dir, paths) = environment();
        fs::create_dir_all(paths.pm3().join("proxmark3")).unwrap();

        let error = run(&Fake::with(""), &silent_ui(), &paths).unwrap_err();

        assert!(matches!(error, AutobuildError::ScriptMissing { .. }));
    }

    /// Nothing to build must not install packages or touch the mount table.
    #[test]
    fn an_empty_home_directory_stops_before_anything_is_changed() {
        let (_dir, paths) = environment();
        crate::assets::install(&paths.msys2(), &silent_ui()).unwrap();
        fs::create_dir_all(paths.pm3()).unwrap();
        let fake = Fake::with("");

        assert_eq!(run(&fake, &silent_ui(), &paths).unwrap(), 0);

        assert!(fake.calls().is_empty(), "got: {:?}", fake.calls());
        assert!(!paths.msys2().join(FSTAB_PATH).exists());
    }
}
