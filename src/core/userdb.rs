//! The Windows account, as the msys2 account files describe it.
//!
//! `mkpasswd` and `mkgroup` report the logged-in user; what they said and what
//! `/etc/passwd` should hold because of it are worked out here, without
//! running either tool or writing either file.

use thiserror::Error;

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

/// What `mkpasswd` or `mkgroup` said, when it cannot be read as an account.
///
/// The caller turns this into the error the user sees; it carries only what
/// the text itself can tell.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "cannot make sense of what `{tool}` reported about the current account\n  \\
     expected {expected}, got: {line}"
)]
pub struct Unparsable {
    pub tool: &'static str,
    pub expected: &'static str,
    pub line: String,
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
pub fn parse_current_user(output: &str) -> Result<CurrentUser, Unparsable> {
    let line = first_useful_line(output).ok_or_else(|| Unparsable {
        tool: "mkpasswd -c",
        expected: "one passwd line",
        line: output.trim().to_string(),
    })?;

    let fields: Vec<&str> = line.split(':').collect();
    // name:passwd:uid:gid:gecos:home:shell
    if fields.len() < 7 || fields[3].is_empty() || fields[4].is_empty() {
        return Err(Unparsable {
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
pub fn parse_current_group(output: &str) -> Result<CurrentGroup, Unparsable> {
    let line = first_useful_line(output).ok_or_else(|| Unparsable {
        tool: "mkgroup -c",
        expected: "one group line",
        line: output.trim().to_string(),
    })?;

    let fields: Vec<&str> = line.split(':').collect();
    // name:sid:gid:members
    if fields.len() < 3 || fields[1].is_empty() {
        return Err(Unparsable {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `mkpasswd -c` output, with the account renamed.
    const MKPASSWD: &str = "somebody:unused:1197603:1049089:U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001:/home/somebody:/bin/bash\n";
    /// Real `mkgroup -c` output. The group name has a space in it on a domain
    /// machine, which is why the line is copied rather than rebuilt.
    const MKGROUP: &str = "Domain Users:S-1-5-21-1234567890-987654321-1122334455-513:1049089:\n";

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
                matches!(parse_current_user(bad), Err(Unparsable { .. })),
                "accepted `{bad}` as a passwd line"
            );
        }
        for bad in ["", "no colons here", "name:"] {
            assert!(
                matches!(parse_current_group(bad), Err(Unparsable { .. })),
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
        assert!(matches!(parse_current_user(no_sid), Err(Unparsable { .. })));
    }
}
