//! Turning a downloaded archive into a tree that can be used.
//!
//! Unpacking it, creating what the base archive leaves out, writing the assets
//! and the account files, and — when a pacman upgrade has moved the runtime —
//! rebasing its DLLs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::core::fstab::Mounts;
use crate::core::msys2::ArchiveSource;
use crate::core::paths::Paths;
use crate::core::state::{Msys2Info, Stage, State, timestamp};
use crate::infra::archive;
use crate::infra::assets::{self as asset_files, AssetError};
use crate::infra::msys2::archive::{Msys2Error, discard_archive, ensure_archive};
use crate::infra::msys2::fstab::{self, FstabError};
use crate::infra::msys2::procs;
use crate::infra::msys2::procs::ProcsError;
use crate::infra::msys2::userdb::{self, UserDbError};
use crate::infra::state as state_file;
use crate::ports::command::{Cmd, CommandError, CommandRunner};
use crate::ports::http::HttpClient;
use crate::ui::Ui;
use crate::ui::interrupt::{self, Interrupted};

/// Get the msys2 tree in place, resuming whatever a previous run left behind.
///
/// The order of the last three steps is the whole point of this function:
/// unpack, *then* record the stage, *then* delete the archive. Every way a run
/// can die in the middle leaves a state that the next run reads correctly:
///
/// - died during the download — the stage is at most `Downloaded`, and the
///   partial file is continued;
/// - died during the unpacking — the tree is still a `.partial` directory and
///   the stage is `Downloaded`, so the archive is still there to unpack again;
/// - died between the rename and the state file — the stage still says
///   `Downloaded`, so the finished tree is thrown away and unpacked again from
///   the archive that has not been deleted yet. Redundant work, but never a
///   tree of unknown provenance;
/// - died between the state file and deleting the archive — the stage says
///   `Extracted` and the leftover archive is cleaned up on the next run.
///
/// What it deliberately does not do is decide whether an *existing* tree should
/// be replaced because a newer msys2 shipped: that decision needs the whole
/// picture and belongs with the rest of the install pipeline.
pub fn ensure_tree(
    client: &dyn HttpClient,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    source: &ArchiveSource,
) -> Result<(), Msys2Error> {
    let tree = paths.msys2();
    let state_path = paths.state_file();

    if state.stage >= Stage::Extracted {
        if tree.is_dir() {
            // The one case where an archive may still be lying around next to a
            // finished install: the previous run died before deleting it.
            discard_archive(ui, paths, source);
            return Ok(());
        }
        ui.warn(&format!(
            "the state file says msys2 is installed, but `{}` is not there; installing it again",
            tree.display()
        ));
        // Whatever the state recorded described the tree that is gone: the
        // packages in it and the python extras added to it went with it.
        state.forget_msys2()?;
        state_file::save(state, &state_path)?;
    }

    let archive = ensure_archive(client, ui, paths, source)?;
    state.move_to(Stage::Downloaded)?;
    state_file::save(state, &state_path)?;

    // Anything at the destination now is the wreckage of an unpacking that
    // never finished — the state file would say `Extracted` otherwise.
    if tree.exists() {
        ui.detail(&format!(
            "removing an unfinished msys2 tree in `{}`",
            tree.display()
        ));
        archive::remove_tree(&tree)?;
    }

    ui.step("unpacking msys2");
    let extracted = archive::extract_stripping_root(&archive, &tree, ui, "unpacking")?;
    ui.detail(&format!(
        "{} entries from `{}/`",
        extracted.entries, extracted.root
    ));

    state.msys2 = Some(Msys2Info {
        version: source.version.clone(),
        source_url: source.url.clone(),
        sha256: source.sha256.clone(),
        extracted_at: timestamp(),
    });
    state.install_path = Some(paths.base().to_string_lossy().into_owned());
    state.move_to(Stage::Extracted)?;
    state_file::save(state, &state_path)?;

    discard_archive(ui, paths, source);
    ui.success(&format!("msys2 unpacked into `{}`", tree.display()));
    Ok(())
}

/// Directories msys2 needs but does not create for itself. The base archive
/// ships without them and `/etc/profile` assumes both are there.
///
/// `otp/`, which `setup.cmd` also created, is deliberately not here: nothing in
/// any script of the original ever refers to it.
const REQUIRED_DIRS: &[&str] = &["tmp", "dev"];

#[derive(Debug, Error)]
pub enum PrepareError {
    #[error("`{path}` is not there; msys2 has not been unpacked yet")]
    TreeMissing { path: PathBuf },
    #[error("cannot create `{path}`")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Assets(#[from] AssetError),
    #[error(transparent)]
    Fstab(#[from] FstabError),
    #[error(transparent)]
    UserDb(#[from] UserDbError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
}

/// Everything [`prepare`] found or changed.
#[derive(Debug)]
pub struct Prepared {
    pub directories: Vec<PathBuf>,
    pub assets: asset_files::Report,
    pub fstab_changed: bool,
    pub userdb: userdb::Written,
}

impl Prepared {
    /// Whether the tree was actually touched. A quiet `false` on every run
    /// after the first is the point of the whole function.
    pub fn changed_anything(&self) -> bool {
        !self.directories.is_empty()
            || self.assets.changed_anything()
            || self.fstab_changed
            || self.userdb.changed
    }
}

/// Bring an unpacked msys2 tree to the state ProxSpace expects.
///
/// This is `setup/setup.cmd`, minus the parts that turned out to be either
/// pointless or actively unwanted: no `otp/`, no deleting `/etc/passwd` only to
/// write the same thing back, and no `rebaseall` — that one now happens only
/// when asked for, because it took a minute off every single start to fix a
/// problem most installs never have.
///
/// Cheap and idempotent by design, so that every command can call it first
/// without anyone having to reason about whether this particular one needs to.
/// The way it stays cheap is that each step compares before it writes.
pub fn prepare(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    mounts: &Mounts,
) -> Result<Prepared, PrepareError> {
    let tree = paths.msys2();
    if !tree.is_dir() {
        return Err(PrepareError::TreeMissing { path: tree });
    }

    // Asked before anything is written, because the answer comes from two
    // external programs and failing after a half-written tree would be worse.
    let account = userdb::query(runner, ui, &tree)?;
    prepare_with_account(ui, paths, mounts, &account.passwd, &account.group)
}

/// The same, for an account already looked up.
///
/// Exists so the whole of the provisioning can be exercised on a directory that
/// only looks like an msys2 tree: `mkpasswd.exe` cannot be faked, and a step
/// this central should not be testable only on a machine with a real install.
pub fn prepare_with_account(
    ui: &Ui,
    paths: &Paths,
    mounts: &Mounts,
    mkpasswd_output: &str,
    mkgroup_output: &str,
) -> Result<Prepared, PrepareError> {
    let tree = paths.msys2();
    if !tree.is_dir() {
        return Err(PrepareError::TreeMissing { path: tree });
    }
    interrupt::check()?;

    let mut directories = Vec::new();
    for name in REQUIRED_DIRS {
        directories.extend(ensure_dir(&tree.join(name))?);
    }
    // $HOME lives outside the tree and is mounted into it; without it the
    // mount resolves to nothing and the first login lands in `/`.
    directories.extend(ensure_dir(&mounts.pm3)?);
    if let Some(builds) = &mounts.builds {
        directories.extend(ensure_dir(builds)?);
    }
    for path in &directories {
        ui.detail(&format!("created `{}`", path.display()));
    }

    let assets = asset_files::install(&tree, ui)?;
    let fstab_changed = fstab::install(&tree, mounts, ui)?;
    let userdb = userdb::install_from(&tree, mkpasswd_output, mkgroup_output, ui)?;

    Ok(Prepared {
        directories,
        assets,
        fstab_changed,
        userdb,
    })
}

/// Create a directory, reporting it only if it was not already there.
fn ensure_dir(path: &Path) -> Result<Option<PathBuf>, PrepareError> {
    if path.is_dir() {
        return Ok(None);
    }
    fs::create_dir_all(path).map_err(|source| PrepareError::CreateDir {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(path.to_path_buf()))
}

#[derive(Debug, Error)]
pub enum RebaseError {
    #[error("`{path}` is missing; the msys2 tree is incomplete")]
    ToolMissing { path: PathBuf },
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Procs(#[from] ProcsError),
}

/// Recompute the load addresses of the msys2 DLLs (`rebaseall`).
///
/// Cygwin's `fork()` needs every DLL to land at the same address in the child
/// as in the parent, and it cannot when two of them want the same range. The
/// symptom is builds failing at random with "unable to remap" — and nothing
/// else looks wrong, which is why the original ran this on every single start.
///
/// Here it runs only for `repair --rebase`. It is slow, it requires that
/// nothing else in the tree is running, and it fixes a problem that most
/// installs never have; paying for it on every start to avoid explaining it
/// once was the wrong trade.
pub fn rebase(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Result<(), RebaseError> {
    let tree = paths.msys2();
    let dash = tree.join("usr/bin/dash.exe");
    if !dash.is_file() {
        return Err(RebaseError::ToolMissing { path: dash });
    }

    // rebaseall rewrites the DLLs in place and refuses, or corrupts them, if
    // anything is using them.
    procs::stop_holders(&tree, ui)?;

    ui.step("rebasing the msys2 DLLs (this takes a while)");
    let spinner = ui.spinner("rebasing");
    // The command is long and chatty and its output — a list of every DLL it
    // touched — matters only when it goes wrong, so it stays in the log unless
    // `--verbose` asks for it.
    let output = runner.run(
        ui,
        &Cmd::new(&dash)
            .arg("/usr/bin/rebaseall")
            .arg("-p")
            // `rebaseall` reads `uname -s` to decide which base address the
            // DLLs may be moved to, and `uname` answers from `MSYSTEM`. A
            // `MSYSTEM` inherited from the shell ProxSpace was started in —
            // `MINGW64` in a Git Bash, say — makes it read the tree as a mingw
            // one and keep the 32-bit default address, which `rebase` then
            // refuses outright. The tree is an msys2 one whoever asks.
            .env("MSYSTEM", "MSYS")
            .label("rebasing the msys2 DLLs")
            .quiet(),
    );
    spinner.finish_and_clear();
    output?.check()?;

    ui.success("msys2 DLLs rebased");
    Ok(())
}
