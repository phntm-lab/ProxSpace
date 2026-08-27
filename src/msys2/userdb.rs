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

use crate::command::{Cmd, CommandError, CommandRunner};
use crate::msys2;
use crate::ui::Ui;

pub const PASSWD_PATH: &str = "etc/passwd";
pub const GROUP_PATH: &str = "etc/group";

/// Account name written into `/etc/passwd`, as in the original. The name is
/// cosmetic: cygwin identifies the entry by the SID in the GECOS field.
pub const USER_NAME: &str = "proxspace";
/// Home directory of that account, mounted from `<install>/pm3` by `fstab.rs`.
pub const HOME_DIR: &str = "/pm3";
pub const SHELL: &str = "/bin/bash";
/// Fixed uid, straight from the original. It has no meaning on Windows, where
/// access is decided by the token rather than by this number.
pub const UID: &str = "1001";

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
    #[error(
        "cannot make sense of what `{tool}` reported about the current account\n  \
         expected {expected}, got: {line}"
    )]
    Unparsable {
        tool: &'static str,
        expected: &'static str,
        line: String,
    },
    #[error("cannot write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The Windows account, as `mkpasswd -c` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    /// Primary group id, carried over verbatim so that the passwd and group
    /// entries agree with each other.
    pub gid: String,
    /// The GECOS field: `U-HOST\name,S-1-5-21-…`. This is the identity cygwin
    /// matches against the running token.
    pub gecos: String,
}

/// The primary group, as `mkgroup -c` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentGroup {
    /// The whole line, written to `/etc/group` unchanged.
    pub line: String,
    /// The group SID, second field of that line.
    pub sid: String,
}

/// Pull the account out of `mkpasswd -c` output.
pub fn parse_current_user(output: &str) -> Result<CurrentUser, UserDbError> {
    let line = first_useful_line(output).ok_or_else(|| UserDbError::Unparsable {
        tool: "mkpasswd -c",
        expected: "one passwd line",
        line: output.trim().to_string(),
    })?;

    let fields: Vec<&str> = line.split(':').collect();
    // name:passwd:uid:gid:gecos:home:shell
    if fields.len() < 7 || fields[3].is_empty() || fields[4].is_empty() {
        return Err(UserDbError::Unparsable {
            tool: "mkpasswd -c",
            expected: "seven colon-separated fields with a gid and a SID",
            line: line.to_string(),
        });
    }

    Ok(CurrentUser {
        gid: fields[3].to_string(),
        gecos: fields[4].to_string(),
    })
}

/// Pull the primary group out of `mkgroup -c` output.
pub fn parse_current_group(output: &str) -> Result<CurrentGroup, UserDbError> {
    let line = first_useful_line(output).ok_or_else(|| UserDbError::Unparsable {
        tool: "mkgroup -c",
        expected: "one group line",
        line: output.trim().to_string(),
    })?;

    let fields: Vec<&str> = line.split(':').collect();
    // name:sid:gid:members
    if fields.len() < 3 || fields[1].is_empty() {
        return Err(UserDbError::Unparsable {
            tool: "mkgroup -c",
            expected: "at least three colon-separated fields with a SID",
            line: line.to_string(),
        });
    }

    Ok(CurrentGroup {
        line: line.to_string(),
        sid: fields[1].to_string(),
    })
}

/// First line that is neither blank nor a comment. The tools occasionally
/// print a banner before the record.
fn first_useful_line(output: &str) -> Option<&str> {
    output
        .lines()
        .map(str::trim_end)
        .find(|line| !line.trim().is_empty() && !line.starts_with('#') && line.contains(':'))
}

/// The single line of `/etc/passwd`.
pub fn passwd_entry(user: &CurrentUser) -> String {
    format!(
        "{USER_NAME}:unused:{UID}:{}:{}:{HOME_DIR}:{SHELL}",
        user.gid, user.gecos
    )
}

/// Contents of `/etc/passwd`.
///
/// No header comment, unlike the other files ProxSpace generates: this one is
/// parsed by cygwin's own account reader before any shell is running, and it is
/// not worth betting an unusable login on how it treats a line it did not
/// expect.
pub fn render_passwd(user: &CurrentUser) -> String {
    format!("{}\n", passwd_entry(user))
}

/// Contents of `/etc/group`: the line `mkgroup -c` produced, unchanged.
pub fn render_group(group: &CurrentGroup) -> String {
    format!("{}\n", group.line)
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
            Arc::new(crate::logging::Logger::disabled()),
        )
    }

    #[test]
    fn the_account_is_read_out_of_mkpasswd() {
        let user = parse_current_user(MKPASSWD).unwrap();
        assert_eq!(user.gid, "1049089");
        assert_eq!(
            user.gecos,
            "U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001"
        );
    }

    #[test]
    fn the_group_line_is_kept_verbatim() {
        let group = parse_current_group(MKGROUP).unwrap();
        assert_eq!(group.line, MKGROUP.trim_end());
        assert_eq!(group.sid, "S-1-5-21-1234567890-987654321-1122334455-513");
        // The space in "Domain Users" survives: the original echoed the line
        // unquoted and got away with it, but rebuilding the line by field
        // would be one whitespace assumption away from a broken group file.
        assert!(group.line.starts_with("Domain Users:"));
    }

    #[test]
    fn the_passwd_entry_has_the_shape_the_original_produced() {
        let user = parse_current_user(MKPASSWD).unwrap();
        assert_eq!(
            passwd_entry(&user),
            "proxspace:unused:1001:1049089:\
             U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001:/pm3:/bin/bash"
        );
        assert_eq!(passwd_entry(&user).split(':').count(), 7);
    }

    #[test]
    fn home_is_pm3_whoever_is_logged_in() {
        let user = parse_current_user(MKPASSWD).unwrap();
        let entry = passwd_entry(&user);
        let fields: Vec<&str> = entry.split(':').collect();
        assert_eq!(fields[5], "/pm3");
        assert_eq!(fields[6], "/bin/bash");
        assert_eq!(fields[0], "proxspace");
    }

    #[test]
    fn the_sid_travels_in_the_gecos_field() {
        // This is what ties the fixed `proxspace` name to the real Windows
        // token; lose it and every login lands in the wrong home directory.
        let user = parse_current_user(MKPASSWD).unwrap();
        let entry = passwd_entry(&user);
        let fields: Vec<&str> = entry.split(':').collect();
        assert!(fields[4].contains("S-1-5-21-"));
    }

    #[test]
    fn a_banner_before_the_record_is_skipped() {
        let noisy = format!("# this file is generated\n\n{MKPASSWD}");
        assert_eq!(parse_current_user(&noisy).unwrap().gid, "1049089");
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        for bad in ["", "\n\n", "mkpasswd: unknown option", "a:b:c:d"] {
            assert!(
                matches!(parse_current_user(bad), Err(UserDbError::Unparsable { .. })),
                "accepted `{bad}` as a passwd line"
            );
        }
        for bad in ["", "no colons here", "name:"] {
            assert!(
                matches!(
                    parse_current_group(bad),
                    Err(UserDbError::Unparsable { .. })
                ),
                "accepted `{bad}` as a group line"
            );
        }
    }

    #[test]
    fn a_record_without_a_sid_is_refused() {
        // An account with an empty GECOS field would produce a passwd entry
        // cygwin can never match, and a login into the wrong home directory is
        // worse than an error naming the reason.
        let no_sid = "somebody:unused:1197603:1049089::/home/somebody:/bin/bash";
        assert!(matches!(
            parse_current_user(no_sid),
            Err(UserDbError::Unparsable { .. })
        ));
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
        let error = install(&crate::command::ProcessRunner, &silent_ui(), dir.path()).unwrap_err();

        assert!(matches!(error, UserDbError::ToolMissing { .. }));
        assert!(error.to_string().contains("mkpasswd.exe"));
    }
}
