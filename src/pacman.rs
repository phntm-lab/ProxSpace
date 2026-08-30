//! Driving pacman, and the one part of `pacman.conf` ProxSpace owns.
//!
//! This is what the original's `ps-setup` did, with three changes that matter:
//!
//! - packages are installed in **one** transaction instead of a loop calling
//!   `pacman -S` once per name. The original resolved the dependency graph
//!   sixty times and asked the mirrors for it sixty times over;
//! - what is already installed is read once with `pacman -Q` instead of being
//!   probed with `pacman -Q <name>` per package;
//! - a failure is classified rather than passed through. `pacman` exits 1 for
//!   everything from "no network" to "your keyring is broken", and the sentence
//!   that tells the two apart is somewhere in a hundred lines of output the
//!   user has already scrolled past.
//!
//! The pinned `arm-none-eabi-binutils` is why this module also edits
//! `pacman.conf`. The repository build of it breaks the proxmark3 firmware
//! build, so it is installed from a fixed URL and then held there with
//! `IgnorePkg`. The original never touched `pacman.conf`; the block written
//! here is fenced with markers so that it can be rewritten or removed without
//! disturbing anything else in the file.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::command::{Cmd, CommandError, CommandRunner, Output};
use crate::msys2;
use crate::msys2::procs;
use crate::packages::{PackageList, PkgSpec};
use crate::ui::Ui;

/// Where pacman lives inside the tree.
pub const PACMAN_EXE: &str = "usr/bin/pacman.exe";
/// The configuration file whose `IgnorePkg` block is ours.
pub const CONF_PATH: &str = "etc/pacman.conf";
/// Downloaded repository databases.
pub const SYNC_DB_DIR: &str = "var/lib/pacman/sync";
/// Left behind by a pacman that was killed; blocks every later run.
pub const DB_LOCK: &str = "var/lib/pacman/db.lck";

/// Fences around the block this program owns in `pacman.conf`. They exist so
/// the block can be rewritten idempotently and removed cleanly; without them
/// the only options would be appending forever or rewriting the whole file.
pub const MANAGED_BEGIN: &str = "# >>> proxspace managed >>>";
pub const MANAGED_END: &str = "# <<< proxspace managed <<<";

/// Explains the block to whoever opens `pacman.conf` and wonders.
const MANAGED_NOTE: &str = "# Held at a fixed version: the repository build breaks the \
                            proxmark3 firmware build.\n# Written by proxspace; \
                            edit the package list instead of this block.";

/// Ignoring conflicting files is what the original did (`--overwrite='*'`), and
/// it is still needed: the msys2 repositories ship packages whose files overlap,
/// and without this an install stops halfway with a file-exists error.
///
/// No quotes around the `*`: the original's were for the shell, and there is no
/// shell here — quoting it would make pacman look for a package owning a file
/// literally called `'*'`.
const OVERWRITE_ALL: &str = "--overwrite=*";

#[derive(Debug, Error)]
pub enum PacmanError {
    #[error("`{path}` is not there; msys2 is unpacked but not usable")]
    Missing { path: PathBuf },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("cannot {action} `{path}`")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} failed{}{detail}", .kind.advice())]
    Failed {
        operation: String,
        kind: Failure,
        /// The lines of pacman's own output that say what went wrong.
        detail: String,
    },
    #[error(
        "`{path}` has no `[options]` section, so there is nowhere to keep the \
         package pin; the file is not the one msys2 ships"
    )]
    NoOptionsSection { path: PathBuf },
    #[error(
        "`{path}` has a `{MANAGED_BEGIN}` line with no matching `{MANAGED_END}`; \
         the block was edited by hand and cannot be updated safely — \
         remove what is left of it and run this again"
    )]
    BrokenBlock { path: PathBuf },
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
    pub fn classify(output: &Output) -> Failure {
        let text = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
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
                ": another pacman is running, or one was killed and left its lock \
                 behind — close any open ProxSpace window and try again"
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
    fn flag(self) -> &'static str {
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

/// pacman, as reachable inside one msys2 tree.
pub struct Pacman {
    tree: PathBuf,
    exe: PathBuf,
    conf: PathBuf,
}

impl Pacman {
    pub fn new(tree: &Path) -> Pacman {
        Pacman {
            tree: tree.to_path_buf(),
            exe: tree.join(PACMAN_EXE),
            conf: tree.join(CONF_PATH),
        }
    }

    pub fn conf_path(&self) -> &Path {
        &self.conf
    }

    /// Fail early and clearly rather than letting the runner report a missing
    /// file: at this point in the install a missing pacman means the tree is
    /// incomplete, not that the user typed something wrong.
    fn require(&self) -> Result<(), PacmanError> {
        if self.exe.is_file() {
            Ok(())
        } else {
            Err(PacmanError::Missing {
                path: self.exe.clone(),
            })
        }
    }

    /// A pacman invocation, with the environment msys2 programs need
    /// ([`msys2::tool_env`]) and no chance of a question.
    fn cmd(&self, label: &str) -> Cmd {
        Cmd::new(&self.exe)
            .envs(msys2::tool_env(&self.tree))
            .arg("--noconfirm")
            .label(label.to_string())
    }

    /// Run one pacman command and turn a non-zero exit into a classified error.
    fn run(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        cmd: Cmd,
        operation: &str,
    ) -> Result<Output, PacmanError> {
        self.require()?;
        self.clear_stale_lock(ui);

        let output = runner.run(ui, &cmd)?;
        if output.success() {
            return Ok(output);
        }
        Err(PacmanError::Failed {
            operation: operation.to_string(),
            kind: Failure::classify(&output),
            detail: significant_lines(&output),
        })
    }

    /// Everything installed right now.
    pub fn query_installed(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
    ) -> Result<Installed, PacmanError> {
        let output = self.run(
            runner,
            ui,
            self.cmd("`pacman -Q`").arg("-Q").quiet(),
            "listing the installed packages",
        )?;
        Ok(Installed::parse(&output.stdout))
    }

    /// Install packages from the repositories, in one transaction.
    pub fn install(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        names: &[&str],
        mode: Mode,
    ) -> Result<(), PacmanError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut cmd = self
            .cmd(&format!("installing {}", count(names.len(), "package")))
            .arg("-S")
            .arg(OVERWRITE_ALL);
        if mode == Mode::Needed {
            cmd = cmd.arg("--needed");
        }
        self.run(
            runner,
            ui,
            cmd.args(names.iter().copied()),
            "installing packages",
        )?;
        Ok(())
    }

    /// Install one package from a URL (`pacman -U`), which is how a pinned
    /// version gets in.
    pub fn install_url(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        url: &str,
    ) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("installing a pinned package")
                .arg("-U")
                .arg(OVERWRITE_ALL)
                .arg(url),
            "installing the pinned package",
        )?;
        Ok(())
    }

    /// `pacman -Syuu`: bring the whole installation, msys2 runtime included, to
    /// what the repositories hold.
    ///
    /// Two `u`s on purpose, as in the original: the second allows going
    /// *backwards*, which is what makes a tree recover after a repository
    /// rolled a package back.
    pub fn system_upgrade(&self, runner: &dyn CommandRunner, ui: &Ui) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("updating msys2").arg("-Syuu").arg(OVERWRITE_ALL),
            "updating msys2",
        )?;
        Ok(())
    }

    /// Throw away the downloaded repository databases.
    ///
    /// Done before the first `-Syuu` because the databases in the base archive
    /// are as old as the archive and were signed by keys the freshly-created
    /// keyring may not have yet; the symptoms are signature errors and
    /// "database file for ... does not exist", both of which look like
    /// something much worse than "fetch them again".
    ///
    /// A filesystem operation rather than `pacman -Syy`: the point is to be
    /// able to do it when pacman itself is what is failing.
    pub fn reset_sync_db(&self, ui: &Ui) -> Result<(), PacmanError> {
        let dir = self.tree.join(SYNC_DB_DIR);
        if !dir.is_dir() {
            return Ok(());
        }
        let entries = fs::read_dir(&dir).map_err(|source| PacmanError::Io {
            action: "read",
            path: dir.clone(),
            source,
        })?;

        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path).map_err(|source| PacmanError::Io {
                    action: "delete",
                    path: path.clone(),
                    source,
                })?;
                removed += 1;
            }
        }
        if removed > 0 {
            ui.detail(&format!(
                "removed {} from `{}`",
                count(removed, "stale database file"),
                dir.display()
            ));
        }
        Ok(())
    }

    /// Drop downloaded packages, which are gigabytes on disk.
    pub fn clean_cache(
        &self,
        runner: &dyn CommandRunner,
        ui: &Ui,
        scope: Cache,
    ) -> Result<(), PacmanError> {
        self.run(
            runner,
            ui,
            self.cmd("cleaning the package cache").arg(scope.flag()),
            "cleaning the package cache",
        )?;
        Ok(())
    }

    /// Delete a lock left by a pacman that was killed.
    ///
    /// Only when nothing from the tree is running: a lock with a live pacman
    /// behind it is doing its job, and removing it would let two transactions
    /// write the same database. Failing to remove it is not fatal — the pacman
    /// that follows will say so itself, and by then the classification has an
    /// explanation ready.
    fn clear_stale_lock(&self, ui: &Ui) {
        let lock = self.tree.join(DB_LOCK);
        if !lock.exists() {
            return;
        }
        let holders = procs::find_holders(&self.tree);
        if !holders.is_empty() {
            ui.detail(&format!(
                "`{}` is held by {}",
                lock.display(),
                holders
                    .iter()
                    .map(procs::Holder::describe)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        match fs::remove_file(&lock) {
            Ok(()) => ui.detail(&format!(
                "removed the stale lock `{}` left by an interrupted pacman",
                lock.display()
            )),
            Err(error) => ui.detail(&format!("cannot remove `{}` ({error})", lock.display())),
        }
    }

    /// Write, update or remove the `IgnorePkg` block in `pacman.conf`.
    ///
    /// Returns whether the file changed, so that a run which changes nothing
    /// says nothing. An empty list removes the block entirely — that is how a
    /// pin that has been dropped from the package list stops being a pin.
    pub fn set_ignored(&self, ui: &Ui, names: &[&str]) -> Result<bool, PacmanError> {
        let current = fs::read_to_string(&self.conf).map_err(|source| PacmanError::Io {
            action: "read",
            path: self.conf.clone(),
            source,
        })?;
        let updated = with_ignores(&current, names, &self.conf)?;
        if updated == current {
            return Ok(false);
        }

        // Written through a temporary file: a half-written `pacman.conf` is an
        // msys2 that cannot install anything, and this runs on a machine that
        // may lose power at any point in a twenty-minute install.
        let temporary = self.conf.with_extension("conf.proxspace-tmp");
        fs::write(&temporary, updated.as_bytes()).map_err(|source| PacmanError::Io {
            action: "write",
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, &self.conf).map_err(|source| PacmanError::Io {
            action: "replace",
            path: self.conf.clone(),
            source,
        })?;

        if names.is_empty() {
            ui.detail(&format!(
                "removed the pin block from `{}`",
                self.conf.display()
            ));
        } else {
            ui.detail(&format!(
                "pinned {} in `{}`",
                names.join(", "),
                self.conf.display()
            ));
        }
        Ok(true)
    }

    /// The names currently pinned by our block.
    pub fn ignored(&self) -> Result<Vec<String>, PacmanError> {
        let text = fs::read_to_string(&self.conf).map_err(|source| PacmanError::Io {
            action: "read",
            path: self.conf.clone(),
            source,
        })?;
        Ok(managed_ignores(&text))
    }
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
pub fn with_ignores(conf: &str, names: &[&str], path: &Path) -> Result<String, PacmanError> {
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
        return Err(PacmanError::BrokenBlock {
            path: path.to_path_buf(),
        });
    }

    let Some(block) = block else {
        return Ok(conf.to_string());
    };
    let insert_at = end_of_options(conf).ok_or_else(|| PacmanError::NoOptionsSection {
        path: path.to_path_buf(),
    })?;

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
fn significant_lines(output: &Output) -> String {
    let text = format!("{}\n{}", output.stdout, output.stderr);
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
fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("1 {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_CONF: &str = include_str!("../tests/fixtures/pacman.conf");
    const PIN: &str = "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils";

    fn conf_path() -> PathBuf {
        PathBuf::from("etc/pacman.conf")
    }

    fn output(stdout: &str, stderr: &str) -> Output {
        Output::new(Some(1), stdout, stderr, "`pacman`")
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
        let updated = with_ignores(REAL_CONF, &[PIN], &conf_path()).unwrap();

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
        let updated = with_ignores(REAL_CONF, &[PIN], &conf_path()).unwrap();
        assert!(updated.find(MANAGED_BEGIN).unwrap() > updated.find("# [staging]").unwrap());
    }

    #[test]
    fn writing_the_same_block_twice_changes_nothing() {
        let once = with_ignores(REAL_CONF, &[PIN], &conf_path()).unwrap();
        let twice = with_ignores(&once, &[PIN], &conf_path()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn the_block_is_rewritten_in_place_when_the_pin_changes() {
        let once = with_ignores(REAL_CONF, &[PIN], &conf_path()).unwrap();
        let changed = with_ignores(&once, &["a", "b"], &conf_path()).unwrap();

        assert_eq!(
            managed_ignores(&changed),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(changed.matches(MANAGED_BEGIN).count(), 1);
        assert!(!changed.contains(PIN));
    }

    #[test]
    fn an_empty_list_removes_the_block_and_leaves_the_file_as_it_was() {
        let with = with_ignores(REAL_CONF, &[PIN], &conf_path()).unwrap();
        let without = with_ignores(&with, &[], &conf_path()).unwrap();

        assert!(!without.contains(MANAGED_BEGIN));
        assert!(managed_ignores(&without).is_empty());
        assert_eq!(
            without, REAL_CONF,
            "removing the block must restore the file"
        );
    }

    #[test]
    fn removing_a_block_that_was_never_there_is_a_no_op() {
        assert_eq!(
            with_ignores(REAL_CONF, &[], &conf_path()).unwrap(),
            REAL_CONF
        );
    }

    #[test]
    fn a_pin_the_user_set_by_hand_is_left_alone() {
        let by_hand = REAL_CONF.replace("#IgnorePkg   =", "IgnorePkg = something-of-theirs");
        let updated = with_ignores(&by_hand, &[PIN], &conf_path()).unwrap();

        assert!(updated.contains("IgnorePkg = something-of-theirs"));
        // Only what is inside our fences counts as ours.
        assert_eq!(managed_ignores(&updated), vec![PIN.to_string()]);
    }

    #[test]
    fn a_half_deleted_block_is_refused_rather_than_guessed_at() {
        let broken = format!("[options]\n{MANAGED_BEGIN}\nIgnorePkg = {PIN}\n");
        let error = with_ignores(&broken, &[PIN], &conf_path()).unwrap_err();
        assert!(matches!(error, PacmanError::BrokenBlock { .. }));
        assert!(error.to_string().contains("edited by hand"));
    }

    #[test]
    fn a_conf_without_an_options_section_is_refused() {
        let error = with_ignores(
            "[ucrt64]\nInclude = /etc/pacman.d/mirrorlist.mingw\n",
            &[PIN],
            &conf_path(),
        )
        .unwrap_err();
        assert!(matches!(error, PacmanError::NoOptionsSection { .. }));
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
    fn an_error_says_what_failed_what_to_do_and_what_pacman_printed() {
        let error = PacmanError::Failed {
            operation: "installing packages".to_string(),
            kind: Failure::Network,
            detail: significant_lines(&output(
                "resolving dependencies...\nlooking for conflicting packages...",
                "error: failed retrieving file 'ucrt64.db'\nwarning: too slow",
            )),
        };

        let message = error.to_string();
        assert!(
            message.starts_with("installing packages failed: the mirrors could not be reached")
        );
        assert!(message.contains("error: failed retrieving file 'ucrt64.db'"));
        assert!(message.contains("warning: too slow"));
        // The chatter before the failure is not part of the message.
        assert!(!message.contains("resolving dependencies"));
    }

    #[test]
    fn without_a_marked_line_the_tail_of_the_output_is_used() {
        let detail = significant_lines(&output("first\nsecond\nthird\nfourth\n", ""));
        assert!(detail.contains("fourth"));
        assert!(detail.contains("second"));
        assert!(!detail.contains("first"));
    }

    #[test]
    fn an_unknown_failure_says_only_what_failed() {
        let error = PacmanError::Failed {
            operation: "cleaning the package cache".to_string(),
            kind: Failure::Unknown,
            detail: String::new(),
        };
        assert_eq!(error.to_string(), "cleaning the package cache failed");
    }

    #[test]
    fn things_are_counted_in_words_that_read_properly() {
        assert_eq!(count(1, "package"), "1 package");
        assert_eq!(count(42, "package"), "42 packages");
    }
}
