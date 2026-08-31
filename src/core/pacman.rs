//! Reading what pacman says, and editing the block ProxSpace owns in
//! `pacman.conf`.
//!
//! Every function here is a string in and a value out. Running pacman and
//! writing the file are somebody else's business; what its output means, and
//! what the file should look like afterwards, is decided here.

use std::collections::BTreeMap;

use crate::core::packages::{PackageList, PkgSpec};

/// Fences around the block this program owns in `pacman.conf`. They exist so
/// the block can be rewritten idempotently and removed cleanly; without them
/// the only options would be appending forever or rewriting the whole file.
pub const MANAGED_BEGIN: &str = "# >>> proxspace managed >>>";
pub const MANAGED_END: &str = "# <<< proxspace managed <<<";

/// Explains the block to whoever opens `pacman.conf` and wonders.
const MANAGED_NOTE: &str = "# Held at a fixed version: the repository build breaks the \
                            proxmark3 firmware build.\n# Written by proxspace; \
                            edit the package list instead of this block.";

/// What can be wrong with the fenced block in a `pacman.conf`.
///
/// Two cases, both about the file's shape rather than about reading it, which
/// is why they are separate from the errors of running pacman: the caller has
/// the path and turns these into the sentence the user sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreBlockError {
    /// A begin fence with no matching end: edited by hand, and rewriting it
    /// blindly would eat whatever is below.
    Broken,
    /// No `[options]` section, so there is nowhere the block would be read.
    NoOptionsSection,
}

/// What went wrong, as far as it can be told from what pacman printed.
///
/// pacman exits 1 for almost everything, so the exit code is nearly useless on
/// its own; the distinction that matters to the user is in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Network,
    /// Signature or keyring trouble — the classic first-run failure on a base
    /// archive whose keyring was never initialised.
    Signature,
    FileConflict,
    /// A name in the package list that no repository has.
    NotFound,
    /// Another pacman is running, or one died and left its lock behind.
    Locked,
    /// The downloaded databases are stale or damaged.
    StaleDatabase,
    NoSpace,
    Unknown,
}

impl Failure {
    /// Read pacman's own words. Ordered from the most specific cause to the
    /// least: an interrupted download and a broken keyring both mention
    /// "failed to commit transaction", and only one of them is worth advice.
    pub fn classify(output: &str) -> Failure {
        let text = output.to_lowercase();
        let says = |needle: &str| text.contains(needle);

        if says("unable to lock database") || says("db.lck") {
            Failure::Locked
        } else if says("no space left") || says("not enough free disk space") {
            Failure::NoSpace
        } else if says("unknown trust")
            || says("signature from")
            || says("invalid or corrupted package")
            || says("marginal trust")
        {
            Failure::Signature
        } else if says("could not resolve host")
            || says("failed retrieving file")
            || says("unable to connect")
            || says("failed to synchronize")
            || says("connection timed out")
        {
            Failure::Network
        } else if says("exists in filesystem") || says("conflicting files") {
            Failure::FileConflict
        } else if says("target not found") || says("could not find or read package") {
            Failure::NotFound
        } else if says("database file for") || says("could not open file") {
            Failure::StaleDatabase
        } else {
            Failure::Unknown
        }
    }

    /// What to tell the user, as a clause following "… failed".
    pub fn advice(self) -> &'static str {
        match self {
            Failure::Network => {
                ": the mirrors could not be reached — check the network or a proxy, \
                 then run the same command again"
            }
            Failure::Signature => {
                ": a package signature could not be verified — the pacman keyring is \
                 not initialised or is out of date; `proxspace repair` rebuilds it"
            }
            Failure::FileConflict => {
                ": two packages claim the same file, and even `--overwrite` did not \
                 settle it — `proxspace repair` reinstalls the environment over itself"
            }
            Failure::NotFound => {
                ": a package in the list is not in any repository — the list and the \
                 repositories have drifted apart, which is a bug in this build"
            }
            Failure::Locked => {
                ": another pacman is running — close any other ProxSpace window and \
                 run the same command again; a lock left behind by one that was \
                 stopped is removed by the next run once nothing holds it"
            }
            Failure::StaleDatabase => {
                ": the package databases are damaged — `proxspace repair` fetches \
                 them again"
            }
            Failure::NoSpace => ": the disk is full",
            Failure::Unknown => "",
        }
    }
}

/// What `pacman -Q` reported: every installed package and its version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    packages: BTreeMap<String, String>,
}

impl Installed {
    /// Read the output of `pacman -Q`: one `name version` per line.
    ///
    /// Anything that is not two fields is skipped rather than refused: pacman
    /// prefixes warnings to the same stream, and one stray line must not cost
    /// the whole picture.
    pub fn parse(text: &str) -> Installed {
        let packages = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                let version = fields.next()?;
                // A third field means this is not a package line at all, and a
                // colon in the name means it is a diagnostic: pacman prefixes
                // those with `error:` or `warning:`, and no package name has a
                // colon in it (an epoch belongs to the version).
                let is_package = fields.next().is_none() && !name.contains(':');
                is_package.then(|| (name.to_string(), version.to_string()))
            })
            .collect();
        Installed { packages }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }

    pub fn version(&self, name: &str) -> Option<&str> {
        self.packages.get(name).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Every installed package name — what `repair` reinstalls over itself.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.packages.keys().map(String::as_str)
    }

    /// Entries of the list that are not installed at all.
    pub fn missing<'a>(&self, list: &'a PackageList) -> Vec<&'a PkgSpec> {
        list.specs()
            .iter()
            .filter(|spec| !self.contains(spec.name()))
            .collect()
    }

    /// Pinned entries that are installed at some other version.
    ///
    /// The safety net behind `IgnorePkg`: the pin can still be walked over —
    /// by `--overwrite`, by the package
    /// arriving as a dependency, or by someone editing `pacman.conf` — and the
    /// only way to know is to look at what is actually installed.
    pub fn stale_pins<'a>(&self, list: &'a PackageList) -> Vec<&'a PkgSpec> {
        list.pinned()
            .filter(|spec| match self.version(spec.name()) {
                Some(installed) => Some(installed) != spec.version(),
                // Absent, not stale: that is `missing`'s business.
                None => false,
            })
            .collect()
    }
}

/// How much of the download cache to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    /// `pacman -Sc`: only what is no longer installed. Run at the end of an
    /// install, where it frees the superseded downloads and leaves the files
    /// of the current versions — which is what lets a repair work offline.
    Superseded,
    /// `pacman -Scc`: everything, the current versions included. The only one
    /// that frees anything on a tree that has just been installed, and the
    /// reason it costs a download is exactly why it is asked for by hand.
    All,
}

impl Cache {
    pub fn flag(self) -> &'static str {
        match self {
            Cache::Superseded => "-Sc",
            Cache::All => "-Scc",
        }
    }
}

/// Whether an install may skip packages that are already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `--needed`: leave installed packages alone. The normal case.
    Needed,
    /// Install over whatever is there. This is what `repair` and `--force` are.
    Reinstall,
}

/// The `IgnorePkg` names inside our fenced block, ignoring any the user set
/// elsewhere in the file — those are theirs and are none of our business.
pub fn managed_ignores(conf: &str) -> Vec<String> {
    let Some((_, inside)) = split_block(conf) else {
        return Vec::new();
    };
    inside
        .lines()
        .filter_map(|line| line.trim().strip_prefix("IgnorePkg"))
        .flat_map(|rest| {
            rest.trim_start()
                .strip_prefix('=')
                .unwrap_or(rest)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `pacman.conf` with our block set to exactly `names`.
///
/// The block has to live inside `[options]` — `IgnorePkg` in a repository
/// section is not an error, it is simply ignored — so a first write goes in at
/// the end of that section rather than at the end of the file.
pub fn with_ignores(conf: &str, names: &[&str]) -> Result<String, IgnoreBlockError> {
    let block = (!names.is_empty()).then(|| {
        format!(
            "{MANAGED_BEGIN}\n{MANAGED_NOTE}\nIgnorePkg = {}\n{MANAGED_END}\n",
            names.join(" ")
        )
    });

    if let Some((range, _)) = split_block(conf) {
        let mut updated = String::with_capacity(conf.len() + 256);
        updated.push_str(&conf[..range.0]);
        if let Some(block) = &block {
            updated.push_str(block);
            updated.push('\n');
        }
        updated.push_str(&conf[range.1..]);
        return Ok(updated);
    }
    if conf.contains(MANAGED_BEGIN) || conf.contains(MANAGED_END) {
        return Err(IgnoreBlockError::Broken);
    }

    let Some(block) = block else {
        return Ok(conf.to_string());
    };
    let insert_at = end_of_options(conf).ok_or(IgnoreBlockError::NoOptionsSection)?;

    let mut updated = String::with_capacity(conf.len() + block.len() + 2);
    updated.push_str(&conf[..insert_at]);
    updated.push_str(&block);
    updated.push('\n');
    updated.push_str(&conf[insert_at..]);
    Ok(updated)
}

/// Byte range of the fenced block, and the text between the fences.
fn split_block(conf: &str) -> Option<((usize, usize), &str)> {
    let begin = conf.find(MANAGED_BEGIN)?;
    let end = conf[begin..].find(MANAGED_END)? + begin;
    let mut after = conf[end..]
        .find('\n')
        .map_or(conf.len(), |offset| end + offset + 1);
    // The blank line written after the block belongs to the block: taking it
    // along is what makes removing the block restore the file exactly.
    if conf[after..].starts_with('\n') {
        after += 1;
    }
    let inside = &conf[begin + MANAGED_BEGIN.len()..end];
    Some(((begin, after), inside))
}

/// Offset of the line that ends the `[options]` section: the next section
/// header, or the end of the file. Commented-out headers such as `# [staging]`
/// do not count — msys2 ships two of them.
fn end_of_options(conf: &str) -> Option<usize> {
    let options = conf
        .match_indices("[options]")
        .find(|(offset, _)| at_line_start(conf, *offset))?
        .0;

    let mut lines = conf[options..].split_inclusive('\n');
    // Past the `[options]` header itself, which is a section header too.
    let mut offset = options + lines.next().map_or(0, str::len);
    for line in lines {
        if line.trim_start().starts_with('[') {
            return Some(offset);
        }
        offset += line.len();
    }
    Some(conf.len())
}

fn at_line_start(text: &str, offset: usize) -> bool {
    offset == 0 || text.as_bytes()[offset - 1] == b'\n'
}

/// The part of a failed command's output worth putting in an error message:
/// the lines pacman marks as errors or warnings, or failing that the tail.
pub fn significant_lines(output: &str) -> String {
    let text = output;
    let marked: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            let lowered = line.to_lowercase();
            lowered.starts_with("error") || lowered.starts_with("warning")
        })
        .collect();

    let chosen: Vec<&str> = if marked.is_empty() {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        marked.into_iter().take(5).collect()
    };

    chosen.iter().fold(String::new(), |mut text, line| {
        text.push_str("\n  ");
        text.push_str(line);
        text
    })
}

/// "1 package" / "7 packages", so that messages do not read like a form.
pub fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("1 {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_CONF: &str = include_str!("../../tests/fixtures/pacman.conf");
    const PIN: &str = "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils";

    /// Both pacman streams as the readers here see them.
    fn output(stdout: &str, stderr: &str) -> String {
        format!(
            "{stdout}
{stderr}"
        )
    }

    /// The difference is the whole point of the two: `-Sc` on a tree that was
    /// just installed frees nothing at all, because everything in the cache is
    /// installed, and a `clean` that frees nothing is a `clean` that lies.
    #[test]
    fn the_two_cache_scopes_are_the_two_pacman_flags() {
        assert_eq!(Cache::Superseded.flag(), "-Sc");
        assert_eq!(Cache::All.flag(), "-Scc");
    }

    #[test]
    fn installed_packages_are_read_as_name_and_version() {
        let installed = Installed::parse(
            "git 2.51.0-1\nmingw-w64-ucrt-x86_64-gcc 15.2.0-2\nbase-devel 2022.12-1\n",
        );

        assert_eq!(installed.len(), 3);
        assert_eq!(installed.version("git"), Some("2.51.0-1"));
        assert!(installed.contains("base-devel"));
        assert!(!installed.contains("make"));
    }

    #[test]
    fn lines_that_are_not_packages_are_skipped() {
        let installed = Installed::parse(
            "warning: database file for 'ucrt64' does not exist\n\
             git 2.51.0-1\n\
             \n\
             error: something\n",
        );

        assert_eq!(installed.len(), 1);
        assert_eq!(installed.version("git"), Some("2.51.0-1"));
    }

    #[test]
    fn what_is_missing_is_what_is_not_there() {
        let list = PackageList::shipped().unwrap();
        let installed = Installed::parse("git 2.51.0-1\nmake 4.4.1-2\n");

        let missing: Vec<&str> = installed.missing(&list).iter().map(|s| s.name()).collect();

        assert!(!missing.contains(&"git"));
        assert!(!missing.contains(&"make"));
        assert!(missing.contains(&"pkgconf"));
        assert!(missing.contains(&PIN));
        assert_eq!(missing.len(), list.len() - 2);
    }

    #[test]
    fn nothing_is_missing_from_a_complete_installation() {
        let list = PackageList::shipped().unwrap();
        let text: String = list
            .specs()
            .iter()
            .map(|spec| format!("{} {}\n", spec.name(), spec.version().unwrap_or("1.0-1")))
            .collect();

        let installed = Installed::parse(&text);
        assert!(installed.missing(&list).is_empty());
        assert!(installed.stale_pins(&list).is_empty());
    }

    #[test]
    fn a_pin_pulled_forward_by_an_upgrade_is_noticed() {
        let list = PackageList::shipped().unwrap();

        // What `pacman -Syuu` does when `IgnorePkg` is bypassed.
        let installed = Installed::parse(&format!("{PIN} 2.47.0-1\n"));
        let stale: Vec<&str> = installed
            .stale_pins(&list)
            .iter()
            .map(|s| s.name())
            .collect();
        assert_eq!(stale, vec![PIN]);

        // At the pinned version there is nothing to do.
        let installed = Installed::parse(&format!("{PIN} 2.46.1-1\n"));
        assert!(installed.stale_pins(&list).is_empty());

        // And a package that is not installed at all is missing, not stale.
        let installed = Installed::parse("git 2.51.0-1\n");
        assert!(installed.stale_pins(&list).is_empty());
        assert!(installed.missing(&list).iter().any(|s| s.name() == PIN));
    }

    #[test]
    fn the_pin_block_lands_inside_the_options_section() {
        let updated = with_ignores(REAL_CONF, &[PIN]).unwrap();

        let block = updated.find(MANAGED_BEGIN).unwrap();
        let options = updated.find("\n[options]").unwrap();
        let first_repo = updated.find("\n[clangarm64]").unwrap();

        assert!(
            options < block && block < first_repo,
            "the block has to be inside [options], or pacman ignores it"
        );
        assert!(updated.contains(&format!("IgnorePkg = {PIN}")));
        // Nothing else in the file moved.
        assert!(updated.contains("HoldPkg     = pacman"));
        assert!(updated.contains("[ucrt64]"));
        assert_eq!(managed_ignores(&updated), vec![PIN.to_string()]);
    }

    #[test]
    fn a_commented_out_section_header_does_not_end_the_options_section() {
        // msys2 ships `# [staging]` inside the comment block before the repos.
        assert!(REAL_CONF.contains("# [staging]"));
        let updated = with_ignores(REAL_CONF, &[PIN]).unwrap();
        assert!(updated.find(MANAGED_BEGIN).unwrap() > updated.find("# [staging]").unwrap());
    }

    #[test]
    fn writing_the_same_block_twice_changes_nothing() {
        let once = with_ignores(REAL_CONF, &[PIN]).unwrap();
        let twice = with_ignores(&once, &[PIN]).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn the_block_is_rewritten_in_place_when_the_pin_changes() {
        let once = with_ignores(REAL_CONF, &[PIN]).unwrap();
        let changed = with_ignores(&once, &["a", "b"]).unwrap();

        assert_eq!(
            managed_ignores(&changed),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(changed.matches(MANAGED_BEGIN).count(), 1);
        assert!(!changed.contains(PIN));
    }

    #[test]
    fn an_empty_list_removes_the_block_and_leaves_the_file_as_it_was() {
        let with = with_ignores(REAL_CONF, &[PIN]).unwrap();
        let without = with_ignores(&with, &[]).unwrap();

        assert!(!without.contains(MANAGED_BEGIN));
        assert!(managed_ignores(&without).is_empty());
        assert_eq!(
            without, REAL_CONF,
            "removing the block must restore the file"
        );
    }

    #[test]
    fn removing_a_block_that_was_never_there_is_a_no_op() {
        assert_eq!(with_ignores(REAL_CONF, &[]).unwrap(), REAL_CONF);
    }

    #[test]
    fn a_pin_the_user_set_by_hand_is_left_alone() {
        let by_hand = REAL_CONF.replace("#IgnorePkg   =", "IgnorePkg = something-of-theirs");
        let updated = with_ignores(&by_hand, &[PIN]).unwrap();

        assert!(updated.contains("IgnorePkg = something-of-theirs"));
        // Only what is inside our fences counts as ours.
        assert_eq!(managed_ignores(&updated), vec![PIN.to_string()]);
    }

    #[test]
    fn failures_are_told_apart_by_what_pacman_said() {
        let cases = [
            (
                "error: failed retrieving file 'ucrt64.db' from mirror.msys2.org",
                Failure::Network,
            ),
            (
                "error: could not resolve host: repo.msys2.org",
                Failure::Network,
            ),
            (
                "error: gcc: signature from \"CI (Build) <ci@msys2.org>\" is unknown trust",
                Failure::Signature,
            ),
            (
                "error: failed to commit transaction (invalid or corrupted package)",
                Failure::Signature,
            ),
            (
                "error: failed to commit transaction (conflicting files)\nfoo: /usr/bin/x exists in filesystem",
                Failure::FileConflict,
            ),
            (
                "error: target not found: mingw-w64-ucrt-x86_64-nonesuch",
                Failure::NotFound,
            ),
            ("error: unable to lock database", Failure::Locked),
            (
                "error: database file for 'msys' does not exist",
                Failure::StaleDatabase,
            ),
            ("error: not enough free disk space", Failure::NoSpace),
            ("error: something nobody has seen before", Failure::Unknown),
        ];

        for (text, expected) in cases {
            assert_eq!(Failure::classify(&output("", text)), expected, "on: {text}");
        }
    }

    #[test]
    fn a_lock_beats_a_network_message_when_both_appear() {
        // An interrupted pacman leaves a lock *and* a half-finished download,
        // and the lock is the one the user has to act on.
        let text = "error: failed retrieving file 'x'\nerror: unable to lock database";
        assert_eq!(Failure::classify(&output("", text)), Failure::Locked);
    }

    #[test]
    fn without_a_marked_line_the_tail_of_the_output_is_used() {
        let detail = significant_lines(&output("first\nsecond\nthird\nfourth\n", ""));
        assert!(detail.contains("fourth"));
        assert!(detail.contains("second"));
        assert!(!detail.contains("first"));
    }

    #[test]
    fn things_are_counted_in_words_that_read_properly() {
        assert_eq!(count(1, "package"), "1 package");
        assert_eq!(count(42, "package"), "42 packages");
    }
}
