//! `mirrors rank` and `mirrors restore`: which download servers pacman tries,
//! and in what order.
//!
//! This is `ps-rankmirrors` and `ps-restoremirrors`. The shape is the same —
//! keep an untouched copy of the list msys2 shipped, rank *that* rather than
//! the already-ranked file, and be able to put the original back — with three
//! things fixed.
//!
//! The file is `mirrorlist.mingw`: msys2 has one list for every mingw flavour
//! and picks the repository out of it with `$repo`, so the
//! `mirrorlist.mingw64` the original copied and ranked has not existed for
//! years, and both halves of the script were silently doing nothing.
//! `rankmirrors` comes from `pacman-contrib`, which the base install does not
//! have, so it is installed when it is needed instead of failing with
//! "command not found". And the ranked output is checked before it replaces
//! anything: a ranking run without a network prints no servers at all, and
//! writing that over the list would leave an installation that cannot reach a
//! single mirror.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::pacman::Mode;
use crate::core::paths::Paths;
use crate::infra::msys2::{self, shell};
use crate::infra::pacman::{Pacman, PacmanError};
use crate::ports::command::{Cmd, CommandError, CommandRunner};
use crate::ui::Ui;
use crate::ui::interrupt::{self, Interrupted};

/// Where pacman keeps the mirror lists, inside the tree.
pub const MIRRORLIST_DIR: &str = "etc/pacman.d";
/// The ranking tool, and the package that brings it.
const RANKMIRRORS: &str = "usr/bin/rankmirrors";
const RANKMIRRORS_PACKAGE: &str = "pacman-contrib";
/// Suffix of the untouched copy kept beside each list.
const BACKUP_SUFFIX: &str = ".backup";

/// The `$repo` the mingw mirror list stands for in this build: [`MSYSTEM`] in
/// the lowercase form the repository names use. A test keeps the two together.
const MINGW_REPO: &str = "ucrt64";

/// One mirror list, and the repository name `rankmirrors` has to put in place
/// of the `$repo` in it before a URL can be fetched.
struct List {
    file: &'static str,
    repo: &'static str,
}

const LISTS: &[List] = &[
    List {
        file: "mirrorlist.msys",
        repo: "msys",
    },
    List {
        file: "mirrorlist.mingw",
        repo: MINGW_REPO,
    },
];

#[derive(Debug, Error)]
pub enum MirrorsError {
    #[error("`{path}` is not there; msys2 has not been unpacked yet")]
    TreeMissing { path: PathBuf },
    #[error("`{path}` is still missing after installing the package that provides it")]
    ToolMissing { path: PathBuf },
    #[error(
        "ranking the `{repo}` mirrors produced no servers — the network was \
         probably unreachable\n  `{path}` has been left as it was"
    )]
    NoMirrors { repo: String, path: PathBuf },
    #[error("cannot {action} `{path}`")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Pacman(#[from] PacmanError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// The untouched copy kept beside a list.
fn backup_of(list: &Path) -> PathBuf {
    let mut name = list.as_os_str().to_os_string();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

/// The mirror-list directory, or an error naming what is not there.
fn mirrorlist_dir(paths: &Paths) -> Result<PathBuf, MirrorsError> {
    let dir = paths.msys2().join(MIRRORLIST_DIR);
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(MirrorsError::TreeMissing { path: dir })
    }
}

/// Reorder the mirror lists by measured speed.
///
/// Always ranked from the backup, never from the file in use: ranking the
/// output of the last ranking would let one slow measurement push a good mirror
/// down the list for good.
pub fn rank(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Result<(), MirrorsError> {
    let tree = paths.msys2();
    let dir = mirrorlist_dir(paths)?;
    ensure_rankmirrors(runner, ui, &tree)?;
    let bash = shell::bash_path(&tree);

    for list in LISTS {
        // Each list is measured against every mirror it names, which
        // takes long enough that stopping between the two is worth a
        // checkpoint of its own.
        interrupt::check()?;
        let path = dir.join(list.file);
        if !path.is_file() {
            ui.warn(&format!(
                "`{}` is not there; leaving the `{}` mirrors alone",
                path.display(),
                list.repo
            ));
            continue;
        }
        let backup = backup_of(&path);
        if !backup.exists() {
            copy(&path, &backup)?;
            ui.detail(&format!("kept the shipped list as `{}`", backup.display()));
        }

        ui.step(&format!("ranking the `{}` mirrors", list.repo));
        let spinner = ui.spinner("measuring every mirror in turn");
        // The ranked list is the command's output, so it goes to the log and
        // to `--verbose` but not to the screen: it is about to be written to a
        // file, and printing it twice helps nobody.
        let output = runner.run(
            ui,
            &Cmd::new(&bash)
                .arg(posix(RANKMIRRORS))
                .arg("-v")
                .arg("--repo")
                .arg(list.repo)
                .arg(posix(&format!(
                    "{MIRRORLIST_DIR}/{}{BACKUP_SUFFIX}",
                    list.file
                )))
                .envs(msys2::tool_env(&tree))
                .label(format!("ranking the `{}` mirrors", list.repo))
                .quiet(),
        );
        spinner.finish_and_clear();
        let output = output?;
        output.check()?;

        if !names_a_server(&output.stdout) {
            return Err(MirrorsError::NoMirrors {
                repo: list.repo.to_string(),
                path,
            });
        }
        write(&path, &output.stdout)?;
        ui.success(&format!(
            "`{}` now lists {} fastest first",
            list.file,
            count_servers(&output.stdout)
        ));
    }

    ui.info("run `proxspace mirrors restore` to put the shipped order back");
    Ok(())
}

/// Put the lists msys2 shipped back.
///
/// The backup is moved rather than copied, so that the next ranking starts from
/// a list nothing has reordered.
pub fn restore(ui: &Ui, paths: &Paths) -> Result<(), MirrorsError> {
    let dir = mirrorlist_dir(paths)?;
    let mut restored = 0;

    for list in LISTS {
        let path = dir.join(list.file);
        let backup = backup_of(&path);
        if !backup.is_file() {
            ui.detail(&format!("`{}` was never ranked", list.file));
            continue;
        }
        fs::rename(&backup, &path).map_err(|source| MirrorsError::Io {
            action: "restore",
            path: path.clone(),
            source,
        })?;
        ui.info(&format!("`{}` is the shipped list again", list.file));
        restored += 1;
    }

    if restored == 0 {
        ui.info("the mirror lists are the ones msys2 shipped; nothing to restore");
    } else {
        ui.success("the shipped mirror order is back");
    }
    Ok(())
}

/// `rankmirrors`, installing the package that provides it if it is not there.
///
/// It is not part of the ProxSpace package set on purpose: it is needed by one
/// command that most installations never run, and the set is already five
/// gigabytes.
fn ensure_rankmirrors(
    runner: &dyn CommandRunner,
    ui: &Ui,
    tree: &Path,
) -> Result<(), MirrorsError> {
    let path = tree.join(RANKMIRRORS);
    if path.is_file() {
        return Ok(());
    }

    ui.info(&format!(
        "`rankmirrors` is not installed; installing `{RANKMIRRORS_PACKAGE}` first"
    ));
    Pacman::new(tree).install(runner, ui, &[RANKMIRRORS_PACKAGE], Mode::Needed)?;

    if path.is_file() {
        Ok(())
    } else {
        Err(MirrorsError::ToolMissing { path })
    }
}

/// Copy a file, naming what failed rather than leaving an `os error 5`.
fn copy(from: &Path, to: &Path) -> Result<(), MirrorsError> {
    fs::copy(from, to).map_err(|source| MirrorsError::Io {
        action: "copy",
        path: to.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), MirrorsError> {
    fs::write(path, text).map_err(|source| MirrorsError::Io {
        action: "write",
        path: path.to_path_buf(),
        source,
    })
}

/// A path inside the tree, as the shell sees it. The tree is `/` in there, so
/// no conversion is needed beyond the leading slash.
fn posix(relative: &str) -> String {
    format!("/{relative}")
}

/// Whether ranked output actually names a mirror.
///
/// `rankmirrors` prints its timings as comments and, with no network, prints
/// nothing else at all. Writing that over a mirror list would leave pacman with
/// nowhere to download from and no way to say why.
fn names_a_server(ranked: &str) -> bool {
    count_servers(ranked) > 0
}

fn count_servers(ranked: &str) -> usize {
    ranked
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("Server"))
        .count()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::core::msys2::MSYSTEM;
    use crate::ports::command::Output;
    use crate::ui::UiOptions;
    use crate::ui::logging::Logger;

    const SHIPPED: &str = "# See https://www.msys2.org/dev/mirrors\n\
         Server = https://mirror.msys2.org/msys/$arch/\n\
         Server = https://repo.msys2.org/msys/$arch/\n";
    const RANKED: &str = "# 0.14 https://repo.msys2.org/\n\
         Server = https://repo.msys2.org/msys/$arch/\n\
         Server = https://mirror.msys2.org/msys/$arch/\n";

    fn silent_ui() -> Ui {
        Ui::new(
            UiOptions {
                quiet: true,
                assume_yes: true,
                ..UiOptions::default()
            },
            Arc::new(Logger::disabled()),
        )
    }

    /// A `rankmirrors` that answers with whatever the test put in it.
    struct Fake {
        stdout: String,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn answering(stdout: &str) -> Fake {
            Fake {
                stdout: stdout.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for Fake {
        fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
            self.calls.lock().unwrap().push(cmd.command_line());
            Ok(Output::new(
                Some(0),
                self.stdout.clone(),
                "",
                cmd.describe(),
            ))
        }
    }

    /// A tree with both mirror lists and the tools the ranking needs.
    fn tree() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::infra::paths::from_dir(dir.path()).unwrap();
        let tree = paths.msys2();

        fs::create_dir_all(tree.join(MIRRORLIST_DIR)).unwrap();
        fs::create_dir_all(tree.join("usr/bin")).unwrap();
        for list in LISTS {
            fs::write(tree.join(MIRRORLIST_DIR).join(list.file), SHIPPED).unwrap();
        }
        for tool in [RANKMIRRORS, shell::BASH] {
            fs::write(tree.join(tool), b"not really a program").unwrap();
        }
        (dir, paths)
    }

    fn read(paths: &Paths, name: &str) -> String {
        fs::read_to_string(paths.msys2().join(MIRRORLIST_DIR).join(name)).unwrap()
    }

    /// The whole reason this command exists in a UCRT64 port: the original
    /// ranked `mirrorlist.mingw64`, which msys2 does not ship.
    #[test]
    fn the_lists_are_the_ones_msys2_actually_ships() {
        let files: Vec<&str> = LISTS.iter().map(|list| list.file).collect();
        assert_eq!(files, ["mirrorlist.msys", "mirrorlist.mingw"]);
        assert!(
            MSYSTEM.eq_ignore_ascii_case(MINGW_REPO),
            "the ranked repository must be the subsystem this build runs in"
        );
    }

    #[test]
    fn a_backup_sits_beside_the_list_it_copies() {
        assert_eq!(
            backup_of(Path::new(
                r"C:\ProxSpace\msys2\etc\pacman.d\mirrorlist.msys"
            )),
            PathBuf::from(r"C:\ProxSpace\msys2\etc\pacman.d\mirrorlist.msys.backup")
        );
    }

    #[test]
    fn ranking_keeps_the_shipped_list_and_writes_the_ranked_one() {
        let (_dir, paths) = tree();

        rank(&Fake::answering(RANKED), &silent_ui(), &paths).unwrap();

        for list in LISTS {
            assert_eq!(read(&paths, list.file), RANKED);
            let backup = format!("{}{BACKUP_SUFFIX}", list.file);
            assert_eq!(read(&paths, &backup), SHIPPED, "no untouched copy kept");
        }
    }

    /// Ranking twice must measure the shipped list both times, or the order
    /// drifts a little further from the truth on every run.
    #[test]
    fn a_second_ranking_still_measures_the_shipped_list() {
        let (_dir, paths) = tree();
        let fake = Fake::answering(RANKED);

        rank(&fake, &silent_ui(), &paths).unwrap();
        rank(&fake, &silent_ui(), &paths).unwrap();

        for list in LISTS {
            let backup = format!("{}{BACKUP_SUFFIX}", list.file);
            assert_eq!(read(&paths, &backup), SHIPPED);
        }
        for call in fake.calls.lock().unwrap().iter() {
            assert!(
                call.contains(BACKUP_SUFFIX),
                "ranked the wrong file: {call}"
            );
        }
    }

    #[test]
    fn each_list_is_ranked_for_its_own_repository() {
        let (_dir, paths) = tree();
        let fake = Fake::answering(RANKED);

        rank(&fake, &silent_ui(), &paths).unwrap();

        let calls = fake.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("--repo msys /etc/pacman.d/mirrorlist.msys.backup"))
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("--repo ucrt64 /etc/pacman.d/mirrorlist.mingw.backup"))
        );
    }

    /// Without a network the ranking prints comments and nothing else. Writing
    /// that out would leave pacman with no mirrors at all.
    #[test]
    fn a_ranking_that_found_nothing_leaves_the_list_alone() {
        let (_dir, paths) = tree();

        let error = rank(
            &Fake::answering("# unreachable https://mirror.msys2.org/\n"),
            &silent_ui(),
            &paths,
        )
        .unwrap_err();

        assert!(matches!(error, MirrorsError::NoMirrors { .. }));
        assert_eq!(read(&paths, "mirrorlist.msys"), SHIPPED);
    }

    #[test]
    fn restoring_puts_the_shipped_lists_back_and_drops_the_backups() {
        let (_dir, paths) = tree();
        rank(&Fake::answering(RANKED), &silent_ui(), &paths).unwrap();

        restore(&silent_ui(), &paths).unwrap();

        for list in LISTS {
            assert_eq!(read(&paths, list.file), SHIPPED);
            assert!(
                !paths
                    .msys2()
                    .join(MIRRORLIST_DIR)
                    .join(format!("{}{BACKUP_SUFFIX}", list.file))
                    .exists(),
                "the backup must not survive a restore"
            );
        }
    }

    #[test]
    fn restoring_a_never_ranked_tree_changes_nothing() {
        let (_dir, paths) = tree();

        restore(&silent_ui(), &paths).unwrap();

        assert_eq!(read(&paths, "mirrorlist.msys"), SHIPPED);
    }

    #[test]
    fn a_tree_that_is_not_there_is_named_rather_than_stumbled_over() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::infra::paths::from_dir(dir.path()).unwrap();

        assert!(matches!(
            restore(&silent_ui(), &paths),
            Err(MirrorsError::TreeMissing { .. })
        ));
        assert!(matches!(
            rank(&Fake::answering(RANKED), &silent_ui(), &paths),
            Err(MirrorsError::TreeMissing { .. })
        ));
    }

    #[test]
    fn only_uncommented_server_lines_count() {
        assert!(names_a_server(RANKED));
        assert_eq!(count_servers(RANKED), 2);
        assert!(!names_a_server("# Server = https://example.test/\n"));
        assert!(!names_a_server(""));
    }
}
