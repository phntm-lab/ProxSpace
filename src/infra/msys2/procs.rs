//! Processes that keep the msys2 tree open, and getting rid of them.
//!
//! Windows will not let a directory be deleted, renamed or overwritten while
//! anything inside it is running or sitting in it, so reinstalling or cleaning
//! the tree fails with a permission error that says nothing about the cause.
//! The usual culprits are the gnupg agents pacman leaves behind
//! (`gpg-agent`, `dirmngr`, `keyboxd`), a shell the user forgot about, or an
//! interrupted `pacman`.
//!
//! The original handled this with one line in `autobuild.bat`:
//! `taskkill /IM "gpg-agent.exe" /F`. Guessing by name misses the shell that is
//! actually holding the tree and hits an unrelated `gpg-agent` from another
//! installation, so here the question asked is the one that matters — is this
//! process running from inside *our* tree, or sitting in it — and the answer
//! comes from the process's own executable path and working directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use thiserror::Error;

use crate::ui::interrupt::{self, Interrupted};
use crate::ui::{Ui, UiError};

/// How long to keep looking after asking the processes to die. Termination is
/// asynchronous on Windows: the call returns before the handles are released.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often to look again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Error)]
pub enum ProcsError {
    #[error(transparent)]
    Ui(#[from] UiError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
    #[error("cancelled: {summary} {verb} still using the msys2 tree")]
    Refused { summary: String, verb: &'static str },
    #[error(
        "{summary} could not be stopped and {verb} still using the msys2 tree\n  \
         close the window it belongs to, or end it in Task Manager, and run this again"
    )]
    StillRunning { summary: String, verb: &'static str },
}

/// Why a process counts as holding the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// It is running one of the tree's own executables.
    Executable,
    /// Its working directory is inside the tree.
    WorkingDirectory,
}

impl Reason {
    pub fn describe(self) -> &'static str {
        match self {
            Reason::Executable => "running from the tree",
            Reason::WorkingDirectory => "working directory inside the tree",
        }
    }
}

/// One process standing in the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub reason: Reason,
    /// Whether it runs as the user running ProxSpace. Anything else belongs to
    /// another session or to a service, and terminating it is not ours to do.
    pub ours: bool,
}

impl Holder {
    /// One line for the list shown before anything is killed.
    pub fn describe(&self) -> String {
        let name = if self.name.is_empty() {
            "(unnamed)"
        } else {
            &self.name
        };
        let owner = if self.ours { "" } else { ", another user" };
        format!(
            "{name} (pid {}, {}{owner})",
            self.pid,
            self.reason.describe()
        )
    }
}

/// Every process currently holding `tree`, ProxSpace itself excluded.
pub fn find_holders(tree: &Path) -> Vec<Holder> {
    let mut system = System::new();
    let refresh = ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::Always)
        .with_cwd(UpdateKind::Always)
        .with_user(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

    let self_pid = sysinfo::get_current_pid().ok();
    let our_user = self_pid
        .and_then(|pid| system.process(pid))
        .and_then(|process| process.user_id())
        .cloned();

    let mut holders: Vec<Holder> = system
        .processes()
        .iter()
        .filter(|(pid, _)| Some(**pid) != self_pid)
        .filter_map(|(pid, process)| {
            let reason = reason_for(tree, process.exe(), process.cwd())?;
            Some(Holder {
                pid: pid.as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                exe: process.exe().map(Path::to_path_buf),
                reason,
                // No owner on either side means we cannot tell them apart; say
                // it is ours, because the alternative is refusing to clean up
                // an install the user plainly owns.
                ours: match (&our_user, process.user_id()) {
                    (Some(ours), Some(theirs)) => ours == theirs,
                    _ => true,
                },
            })
        })
        .collect();

    holders.sort_by_key(|holder| holder.pid);
    holders
}

/// Decide whether a process with this executable and working directory is in
/// the way. Split out from the enumeration because it is the part worth
/// testing: everything else is the operating system's answer, not ours.
fn reason_for(tree: &Path, exe: Option<&Path>, cwd: Option<&Path>) -> Option<Reason> {
    if exe.is_some_and(|exe| is_inside(tree, exe)) {
        return Some(Reason::Executable);
    }
    if cwd.is_some_and(|cwd| is_inside(tree, cwd)) {
        return Some(Reason::WorkingDirectory);
    }
    None
}

/// Whether `path` is the tree itself or something under it.
///
/// Compared component by component rather than as text, so that a sibling
/// directory whose name merely starts with the tree's — `msys2-old` next to
/// `msys2` — is not swept up with it. On Windows the comparison ignores case,
/// because the same directory can be spelled either way and the process table
/// does not promise which spelling it will report.
fn is_inside(tree: &Path, path: &Path) -> bool {
    let mut candidate = path.components();
    for expected in tree.components() {
        match candidate.next() {
            Some(actual) if same_component(expected.as_os_str(), actual.as_os_str()) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(windows)]
fn same_component(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn same_component(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left == right
}

/// What [`stop_holders`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stopped {
    pub killed: Vec<Holder>,
}

impl Stopped {
    pub fn stopped_anything(&self) -> bool {
        !self.killed.is_empty()
    }
}

/// Clear the tree of anything holding it, asking first.
///
/// Asking is not a formality: the processes found here are usually a shell the
/// user is working in, and killing one unannounced loses whatever was in it.
/// `--yes` answers for unattended runs, as everywhere else.
///
/// Processes belonging to another user are reported but never touched — they
/// are another session's business, and the call would fail anyway.
pub fn stop_holders(tree: &Path, ui: &Ui) -> Result<Stopped, ProcsError> {
    let holders = find_holders(tree);
    if holders.is_empty() {
        return Ok(Stopped::default());
    }

    let (ours, theirs): (Vec<Holder>, Vec<Holder>) =
        holders.into_iter().partition(|holder| holder.ours);

    for holder in &theirs {
        ui.warn(&format!(
            "{} is using the msys2 tree and belongs to another user; it will not be stopped",
            holder.describe()
        ));
    }

    if ours.is_empty() {
        return Err(ProcsError::StillRunning {
            summary: summarise(&theirs),
            verb: verb_for(theirs.len()),
        });
    }

    ui.warn(&format!(
        "{} {} using the msys2 tree and must be stopped first:",
        summarise(&ours),
        verb_for(ours.len())
    ));
    // The warning above ends in a colon, so the list it promises has to be on
    // screen and not only in the log: without it the user is told a number and
    // asked to act on it.
    for holder in &ours {
        ui.info(&format!("  {}", holder.describe()));
    }

    if !ui.confirm("Stop them?", true)? {
        return Err(ProcsError::Refused {
            summary: summarise(&ours),
            verb: verb_for(ours.len()),
        });
    }

    kill(&ours);
    wait_until_gone(tree, ui)?;

    ui.detail(&format!("stopped {}", summarise(&ours)));
    Ok(Stopped { killed: ours })
}

/// Terminate the listed processes, looking each one up afresh.
///
/// A pid can be reused between the listing and the kill, so the process is
/// matched by name as well before it is touched.
fn kill(holders: &[Holder]) {
    let mut system = System::new();
    let pids: Vec<Pid> = holders
        .iter()
        .map(|holder| Pid::from_u32(holder.pid))
        .collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing(),
    );

    for holder in holders {
        if let Some(process) = system.process(Pid::from_u32(holder.pid))
            && process.name().to_string_lossy() == holder.name
        {
            process.kill();
        }
    }
}

/// Wait for the tree to actually be free.
///
/// Termination is asynchronous: the handles a process holds are released some
/// time after the call returns, and deleting the tree immediately afterwards
/// can still fail. Waiting for the process to leave the table is the closest
/// observable proxy for "the files are free again".
fn wait_until_gone(tree: &Path, ui: &Ui) -> Result<(), ProcsError> {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    loop {
        interrupt::check()?;
        let left: Vec<Holder> = find_holders(tree)
            .into_iter()
            .filter(|holder| holder.ours)
            .collect();
        if left.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            for holder in &left {
                ui.warn(&format!("still running: {}", holder.describe()));
            }
            return Err(ProcsError::StillRunning {
                summary: summarise(&left),
                verb: verb_for(left.len()),
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// "1 process" / "3 processes". The verb belongs to the sentence around it —
/// baking it in here reads well in one message and turns the next one into
/// "stopped 3 processes are".
fn summarise(holders: &[Holder]) -> String {
    match holders.len() {
        1 => "1 process".to_string(),
        count => format!("{count} processes"),
    }
}

fn verb_for(count: usize) -> &'static str {
    if count == 1 { "is" } else { "are" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        PathBuf::from(r"C:\ProxSpace\msys2")
    }

    #[test]
    fn an_executable_inside_the_tree_is_a_holder() {
        assert_eq!(
            reason_for(
                &tree(),
                Some(Path::new(r"C:\ProxSpace\msys2\usr\bin\gpg-agent.exe")),
                None
            ),
            Some(Reason::Executable)
        );
    }

    #[test]
    fn a_working_directory_inside_the_tree_is_a_holder_too() {
        // A shell started elsewhere but sitting in the tree still pins it.
        assert_eq!(
            reason_for(
                &tree(),
                Some(Path::new(r"C:\Windows\System32\cmd.exe")),
                Some(Path::new(r"C:\ProxSpace\msys2\home"))
            ),
            Some(Reason::WorkingDirectory)
        );
    }

    #[test]
    fn the_executable_wins_when_both_apply() {
        assert_eq!(
            reason_for(
                &tree(),
                Some(Path::new(r"C:\ProxSpace\msys2\usr\bin\bash.exe")),
                Some(Path::new(r"C:\ProxSpace\msys2\home"))
            ),
            Some(Reason::Executable)
        );
    }

    #[test]
    fn unrelated_processes_are_left_alone() {
        assert_eq!(
            reason_for(
                &tree(),
                Some(Path::new(r"C:\Windows\explorer.exe")),
                Some(Path::new(r"C:\Users\someone"))
            ),
            None
        );
        // The gpg-agent of a different msys2 installation: exactly what the
        // original's taskkill by name would have killed.
        assert_eq!(
            reason_for(
                &tree(),
                Some(Path::new(r"D:\OtherThing\msys2\usr\bin\gpg-agent.exe")),
                None
            ),
            None
        );
    }

    #[test]
    fn a_process_with_nothing_known_about_it_is_not_a_holder() {
        assert_eq!(reason_for(&tree(), None, None), None);
    }

    #[test]
    fn a_sibling_with_a_longer_name_is_not_inside() {
        assert!(!is_inside(
            &tree(),
            Path::new(r"C:\ProxSpace\msys2-old\usr\bin\bash.exe")
        ));
        assert!(!is_inside(
            &tree(),
            Path::new(r"C:\ProxSpaceOther\msys2\bin")
        ));
    }

    #[test]
    fn the_tree_itself_counts_as_inside() {
        assert!(is_inside(&tree(), &tree()));
    }

    #[test]
    fn a_path_above_the_tree_is_not_inside() {
        assert!(!is_inside(&tree(), Path::new(r"C:\ProxSpace")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_without_case() {
        assert!(is_inside(
            &tree(),
            Path::new(r"c:\proxspace\MSYS2\usr\bin\bash.exe")
        ));
    }

    #[test]
    fn holders_read_as_sentences() {
        let holder = Holder {
            pid: 4242,
            name: "gpg-agent.exe".to_string(),
            exe: None,
            reason: Reason::Executable,
            ours: true,
        };
        assert_eq!(
            holder.describe(),
            "gpg-agent.exe (pid 4242, running from the tree)"
        );

        let theirs = Holder {
            ours: false,
            ..holder.clone()
        };
        assert!(theirs.describe().ends_with(", another user)"));

        // The count carries no verb of its own: every message supplies the one
        // its own sentence needs, and one that came baked in turned the next
        // message into "stopped 2 processes are".
        let one = [holder.clone()];
        assert_eq!(summarise(&one), "1 process");
        assert_eq!(verb_for(one.len()), "is");
        let two = [holder.clone(), holder];
        assert_eq!(summarise(&two), "2 processes");
        assert_eq!(verb_for(two.len()), "are");
        assert_eq!(
            format!("stopped {}", summarise(&two)),
            "stopped 2 processes"
        );

        assert_eq!(
            ProcsError::Refused {
                summary: summarise(&one),
                verb: verb_for(one.len()),
            }
            .to_string(),
            "cancelled: 1 process is still using the msys2 tree"
        );
        assert!(
            ProcsError::StillRunning {
                summary: summarise(&two),
                verb: verb_for(two.len()),
            }
            .to_string()
            .starts_with("2 processes could not be stopped and are still using the msys2 tree")
        );
    }

    #[test]
    fn an_empty_tree_has_no_holders() {
        // Nothing can be running inside a directory that does not exist, and
        // asking the question must not be expensive or fail.
        let dir = tempfile::tempdir().unwrap();
        assert!(find_holders(&dir.path().join("msys2")).is_empty());
    }

    #[test]
    fn proxspace_never_lists_itself() {
        // The test binary's own executable directory, used as if it were the
        // tree: the current process runs from inside it and must be skipped.
        let exe = std::env::current_exe().unwrap();
        let holders = find_holders(exe.parent().unwrap());
        let self_pid = std::process::id();
        assert!(holders.iter().all(|holder| holder.pid != self_pid));
    }

    /// The real thing, on the one platform it has to work on: a process whose
    /// working directory is in the tree is found and stopped.
    #[cfg(windows)]
    #[test]
    fn a_process_sitting_in_the_tree_is_found_and_stopped() {
        use std::process::{Command, Stdio};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("msys2");
        std::fs::create_dir_all(&tree).unwrap();

        let mut child = Command::new("cmd.exe")
            .args(["/c", "ping", "-n", "30", "127.0.0.1"])
            .current_dir(&tree)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        // The process table is not updated the instant spawn() returns.
        let deadline = Instant::now() + Duration::from_secs(5);
        while find_holders(&tree).is_empty() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }

        let found = find_holders(&tree);
        assert!(!found.is_empty(), "the child process was not found");
        assert!(found.iter().all(|holder| holder.ours));
        assert!(
            found
                .iter()
                .any(|holder| holder.reason == Reason::WorkingDirectory)
        );

        let ui = Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                assume_yes: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::ui::logging::Logger::disabled()),
        );
        let stopped = stop_holders(&tree, &ui).unwrap();

        assert!(stopped.stopped_anything());
        assert!(find_holders(&tree).is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }
}
