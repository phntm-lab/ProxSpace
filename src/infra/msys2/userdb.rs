//! `/etc/passwd` and `/etc/group`: who the shell thinks you are.
//!
//! A port of the original's `user_setup.sh`. With `passwd: files` in
//! `nsswitch.conf` cygwin stops asking Windows about accounts and reads these
//! two files only, which is what makes logins fast and predictable — and what
//! makes writing them our job.
//!
//! The trick, inherited unchanged, is that the account is always called
//! `proxspace` while the GECOS field carries the *real* Windows identity that
//! `mkpasswd -c` reported. Cygwin matches the entry to the running token by
//! that field, so the name is free to be a constant, and the home directory is
//! free to be `/pm3` for everyone.
//!
//! Both files are generated whole rather than appended to. The original
//! appended, but `setup.cmd` deleted them immediately beforehand on every run,
//! so the effect was the same; writing them outright says so honestly and makes
//! a second run a no-op instead of a duplicate entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::userdb::{
    CurrentGroup, CurrentUser, GROUP_PATH, PASSWD_PATH, Unparsable, parse_current_group,
    parse_current_user, render_group, render_passwd,
};
use crate::infra::msys2;
use crate::ports::command::{Cmd, CommandError, CommandRunner};
use crate::ui::Ui;

/// Tools inside the tree that report the current Windows account in the
/// `/etc/passwd` format.
const MKPASSWD: &str = "usr/bin/mkpasswd.exe";
const MKGROUP: &str = "usr/bin/mkgroup.exe";

#[derive(Debug, Error)]
pub enum UserDbError {
    #[error("`{path}` is missing; the msys2 tree is incomplete")]
    ToolMissing { path: PathBuf },
    #[error(transparent)]
    Tool(#[from] CommandError),
    #[error(transparent)]
    Unparsable(#[from] Unparsable),
    #[error("cannot write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What [`install`] found and wrote.
#[derive(Debug, PartialEq, Eq)]
pub struct Written {
    pub user: CurrentUser,
    pub group: CurrentGroup,
    /// False when both files already said exactly this.
    pub changed: bool,
}

/// What the tree's own tools say about the current Windows account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOutput {
    /// Raw stdout of `mkpasswd -c`.
    pub passwd: String,
    /// Raw stdout of `mkgroup -c`.
    pub group: String,
}

/// Ask the tree who the current user is, without writing anything.
pub fn query(
    runner: &dyn CommandRunner,
    ui: &Ui,
    root: &Path,
) -> Result<AccountOutput, UserDbError> {
    Ok(AccountOutput {
        passwd: run_tool(runner, ui, root, MKPASSWD)?,
        group: run_tool(runner, ui, root, MKGROUP)?,
    })
}

/// Ask the tree who the current user is and write both files.
pub fn install(runner: &dyn CommandRunner, ui: &Ui, root: &Path) -> Result<Written, UserDbError> {
    let account = query(runner, ui, root)?;
    install_from(root, &account.passwd, &account.group, ui)
}

/// The same, from output already in hand.
///
/// Separated so that everything after the two external calls — parsing, the
/// shape of both files, and not rewriting what is already right — can be tested
/// without a Windows account or an msys2 tree to ask about it.
pub fn install_from(
    root: &Path,
    mkpasswd_output: &str,
    mkgroup_output: &str,
    ui: &Ui,
) -> Result<Written, UserDbError> {
    let user = parse_current_user(mkpasswd_output)?;
    let group = parse_current_group(mkgroup_output)?;

    let passwd_changed = write_if_different(&root.join(PASSWD_PATH), &render_passwd(&user), ui)?;
    let group_changed = write_if_different(&root.join(GROUP_PATH), &render_group(&group), ui)?;

    Ok(Written {
        user,
        group,
        changed: passwd_changed || group_changed,
    })
}

fn write_if_different(path: &Path, contents: &str, ui: &Ui) -> Result<bool, UserDbError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| UserDbError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if fs::read(path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(false);
    }
    fs::write(path, contents.as_bytes()).map_err(|source| UserDbError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    ui.detail(&format!("wrote `{}`", path.display()));
    Ok(true)
}

/// Run one of the tree's account tools with `-c` and return its stdout.
///
/// These are cygwin programs, but they are called straight from Windows rather
/// than through a shell: they sit next to `msys-2.0.dll`, so they load, and
/// there is no shell yet to run them in — this is what makes the shell usable
/// in the first place.
/// Run one of the account tools and hand back what it printed.
///
/// Through the [`CommandRunner`] like everything else that shells out, which is
/// what lets the whole of `prepare()` be exercised against a directory that
/// only looks like an msys2 tree: `mkpasswd.exe` is the one thing in here that
/// cannot be faked on disk.
fn run_tool(
    runner: &dyn CommandRunner,
    ui: &Ui,
    root: &Path,
    relative: &str,
) -> Result<String, UserDbError> {
    let path = root.join(relative);
    if !path.is_file() {
        return Err(UserDbError::ToolMissing { path });
    }

    let output = runner.run(
        ui,
        &Cmd::new(&path)
            .envs(msys2::tool_env(root))
            .arg("-c")
            .quiet(),
    )?;
    output.check()?;
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Real `mkpasswd -c` output, with the account renamed.
    const MKPASSWD: &str = "somebody:unused:1197603:1049089:U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001:/home/somebody:/bin/bash\n";
    /// Real `mkgroup -c` output. The group name has a space in it on a domain
    /// machine, which is why the line is copied rather than rebuilt.
    const MKGROUP: &str = "Domain Users:S-1-5-21-1234567890-987654321-1122334455-513:1049089:\n";

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::ui::logging::Logger::disabled()),
        )
    }

    #[test]
    fn both_files_are_written() {
        let dir = tempfile::tempdir().unwrap();

        let written = install_from(dir.path(), MKPASSWD, MKGROUP, &silent_ui()).unwrap();

        assert!(written.changed);
        assert_eq!(
            fs::read_to_string(dir.path().join(PASSWD_PATH)).unwrap(),
            render_passwd(&written.user)
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(GROUP_PATH)).unwrap(),
            MKGROUP
        );
    }

    #[test]
    fn a_second_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install_from(dir.path(), MKPASSWD, MKGROUP, &silent_ui()).unwrap();

        let again = install_from(dir.path(), MKPASSWD, MKGROUP, &silent_ui()).unwrap();

        assert!(!again.changed);
    }

    #[test]
    fn a_new_account_replaces_the_old_entry_instead_of_joining_it() {
        // The folder was copied to another machine, or another user ran it.
        // The original appended, and only got away with it because setup.cmd
        // had just deleted the file.
        let dir = tempfile::tempdir().unwrap();
        install_from(dir.path(), MKPASSWD, MKGROUP, &silent_ui()).unwrap();

        let other = MKPASSWD.replace("-1001:", "-1002:");
        install_from(dir.path(), &other, MKGROUP, &silent_ui()).unwrap();

        let passwd = fs::read_to_string(dir.path().join(PASSWD_PATH)).unwrap();
        assert_eq!(passwd.lines().count(), 1);
        assert!(passwd.contains("-1002:"));
    }

    #[test]
    fn a_missing_tool_names_the_file_that_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let error = install(
            &crate::ports::command::ProcessRunner,
            &silent_ui(),
            dir.path(),
        )
        .unwrap_err();

        assert!(matches!(error, UserDbError::ToolMissing { .. }));
        assert!(error.to_string().contains("mkpasswd.exe"));
    }
}
