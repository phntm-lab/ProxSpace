//! What has to happen to an msys2 tree for it to be the one this build wants.
//!
//! Three version strings decide it — what the state file records, what this
//! build ships, and the oldest version `pacman -Syuu` can still bring all the
//! way forward — and one of the answers deletes gigabytes. Nothing here reads
//! the disk, so every row of the table can be checked without a tree.

use crate::core::versions::is_datestamp;

/// Manual control over the one step that destroys something.
///
/// Without either flag the matrix decides on its own. The flags exist because
/// both of its answers can be the wrong one for somebody: a tree at the right
/// version can still be beyond repair, and a reinstall the matrix thinks is
/// warranted is the last thing a user on a slow connection wants to discover
/// after typing `update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reinstall {
    /// No flag given: the versions decide.
    #[default]
    WhenNeeded,
    /// `--reinstall-msys2`: replace the tree whatever the versions say.
    Always,
    /// `--no-reinstall`: never replace the tree; report instead.
    Never,
}

impl Reinstall {
    /// What the two flags mean together. They cannot both be given — clap
    /// refuses that — so the fourth combination never arrives.
    pub fn from_flags(always: bool, never: bool) -> Reinstall {
        match (always, never) {
            (true, _) => Reinstall::Always,
            (_, true) => Reinstall::Never,
            _ => Reinstall::WhenNeeded,
        }
    }
}

/// What has to happen to the msys2 tree for it to be the one this build
/// expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// There is no tree to update.
    Install,
    /// The tree is already the version this build ships.
    UpToDate { version: String },
    /// The tree is newer than what this build ships, which means the binary was
    /// replaced by an older one. Downgrading msys2 is not something pacman does
    /// and not something worth inventing: the newer tree works.
    Newer { installed: String, shipped: String },
    /// Old enough to need updating, new enough for `pacman -Syuu` to do it.
    Upgrade { from: String, to: String },
    /// Too old for `pacman -Syuu` to get all the way: the tree goes and a fresh
    /// one takes its place.
    Reinstall { from: String, to: String },
    /// The tree needs replacing and `--no-reinstall` forbids it. Nothing is
    /// done, and the caller says why rather than doing half of it.
    Blocked { from: String, to: String },
}

impl Update {
    /// Whether nothing at all can be done. Only a refused reinstall gets here:
    /// every other row has at least an in-place upgrade to run.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Update::Blocked { .. })
    }

    /// Whether it throws the tree away, which is what has to be agreed to
    /// before it happens.
    pub fn destroys_the_tree(&self) -> bool {
        matches!(self, Update::Reinstall { .. })
    }

    /// What the user is told is about to happen, in one line.
    pub fn summary(&self) -> String {
        match self {
            Update::Install => {
                "msys2 is not installed here; it will be downloaded and set up".to_string()
            }
            Update::UpToDate { version } => format!(
                "msys2 {version} is the version this ProxSpace ships; \
                 everything installed in it will be brought up to date in place"
            ),
            Update::Newer { installed, shipped } => format!(
                "this tree holds msys2 {installed}, newer than the {shipped} this ProxSpace ships — \
                 it keeps its version, and everything installed in it is brought up to date"
            ),
            Update::Upgrade { from, to } => format!(
                "msys2 {from} will be brought up to {to} in place with `pacman -Syuu`; \
                 nothing in the tree is deleted"
            ),
            // The same row is reached two ways: because the installed version
            // is beyond in-place upgrading, and because `--reinstall-msys2`
            // asked for the tree to go. Only the first is about the version,
            // and saying it about the second reads as nonsense: "20260611 is
            // too old to be upgraded to 20260611".
            Update::Reinstall { from, to } if from == to => format!(
                "msys2 {from} will be deleted and installed afresh at the same version, \
                 and every package with it"
            ),
            Update::Reinstall { from, to } => format!(
                "msys2 {from} is too old to be upgraded to {to} in place: the `msys2` folder will be \
                 deleted and installed afresh, and every package with it"
            ),
            Update::Blocked { from, to } => format!(
                "msys2 {from} would have to be replaced to reach {to}, \
                 which `--no-reinstall` forbids; nothing will be done"
            ),
        }
    }
}

/// The decision itself, over nothing but version strings, so that every row of
/// it can be checked without a five-gigabyte tree on disk.
///
/// `installed` is what the state file records, or `None` when there is no tree.
/// Versions are msys2 datestamps and compare as strings; anything that is not a
/// datestamp — a hand-edited or corrupted state file — is not ordered against
/// the rest, and gets the upgrade, which is the one answer that cannot destroy
/// a working tree.
pub fn decide_update(
    installed: Option<&str>,
    shipped: &str,
    min_compatible: &str,
    reinstall: Reinstall,
) -> Update {
    let Some(installed) = installed else {
        // Nothing to reinstall and nothing to refuse: either flag means the
        // same thing here as no flag at all.
        return Update::Install;
    };
    let (from, to) = (installed.to_string(), shipped.to_string());

    if reinstall == Reinstall::Always {
        return Update::Reinstall { from, to };
    }
    if !is_datestamp(installed) {
        return Update::Upgrade { from, to };
    }
    if installed == shipped {
        return Update::UpToDate { version: from };
    }
    if installed > shipped {
        return Update::Newer {
            installed: from,
            shipped: to,
        };
    }
    if installed >= min_compatible {
        Update::Upgrade { from, to }
    } else if reinstall == Reinstall::Never {
        Update::Blocked { from, to }
    } else {
        Update::Reinstall { from, to }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_translate_into_the_override() {
        assert_eq!(Reinstall::from_flags(false, false), Reinstall::WhenNeeded);
        assert_eq!(Reinstall::from_flags(true, false), Reinstall::Always);
        assert_eq!(Reinstall::from_flags(false, true), Reinstall::Never);
        assert_eq!(Reinstall::default(), Reinstall::WhenNeeded);
    }
}
