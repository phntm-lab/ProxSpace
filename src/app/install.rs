//! The install itself: an ordered pipeline that can be resumed at any point.
//!
//! The original did this in `runme64.bat` plus `ps-setup`, and the shape of it
//! was dictated by a single problem: `pacman -Syuu` replaces `msys-2.0.dll`
//! underneath the shell that started it, so the shell cannot go on to install
//! anything afterwards. The answer there was to launch the shell twice and let
//! a marker file decide what the second one did.
//!
//! Here every step is a separate process and every completed step is written to
//! `state.json`, so the sequence is explicit and a run that dies — a dropped
//! network, a closed window, Ctrl+C — continues from the step after the last
//! one that finished, instead of starting over or, worse, assuming success.
//!
//! The steps, in order:
//!
//! 1. the msys2 tree is downloaded and unpacked (`msys2::ensure_tree`);
//! 2. the tree is turned into a ProxSpace one (`msys2::prepare`) — done on
//!    every run, because it is idempotent and cheap;
//! 3. one login shell is run and immediately exited, which is what makes msys2
//!    initialise its own pacman keyring through its stock `07-pacman-key.post`;
//! 4. the databases are thrown away and `pacman -Syuu` brings the runtime up to
//!    date;
//! 5. the package list is installed in one transaction, the pinned package from
//!    its URL, and the pin is written into `pacman.conf`;
//! 6. the two python packages pacman does not have, then the package cache is
//!    dropped and the install is `Ready`.

use std::path::PathBuf;

use thiserror::Error;

use crate::core::packages::{PackageList, PackagesError, PkgSpec};
use crate::core::paths::Paths;
use crate::core::state::{PackagesInfo, Stage, State, StateError, timestamp};
use crate::core::update::{Reinstall, Update, decide_update};
use crate::infra::archive::{self, ExtractError};
use crate::infra::msys2::procs::{self, ProcsError};
use crate::infra::msys2::shell::BASH;
use crate::infra::msys2::{self, ArchiveSource, Msys2Error, PrepareError, RebaseError, fstab};
use crate::infra::pacman::{Cache, Mode, Pacman, PacmanError};
use crate::infra::state as state_file;
use crate::ports::command::{Cmd, CommandError, CommandRunner};
use crate::ports::http::HttpClient;
use crate::ui::Ui;
use crate::ui::interrupt::Interrupted;

/// The UCRT64 python, which is the one the proxmark3 client runs under.
const PYTHON: &str = "ucrt64/bin/python.exe";

/// Python packages the proxmark3 client needs and no msys2 repository has.
const PIP_EXTRAS: &[&str] = &["ansicolors", "sslcrypto"];

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Msys2(#[from] Msys2Error),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Pacman(#[from] PacmanError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Packages(#[from] PackagesError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Rebase(#[from] RebaseError),
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Procs(#[from] ProcsError),
    #[error(
        "the msys2 tree here is {from}, too old to be brought up to {to} in place, \
         and `--no-reinstall` forbids replacing it\n  \
         run the command again without `--no-reinstall` to delete `msys2` and install it \
         afresh — `pm3` and `builds` are never touched"
    )]
    ReinstallForbidden { from: String, to: String },
    #[error("`{path}` is not there; the msys2 tree is incomplete")]
    ToolMissing { path: PathBuf },
    #[error(
        "`{name}` is installed at {actual} instead of the pinned {expected}, \
         even after being installed again from its URL\n  \
         the proxmark3 firmware will not build against that version; \
         `proxspace repair` reinstalls the environment"
    )]
    PinNotHeld {
        name: String,
        expected: String,
        actual: String,
    },
}

/// What this run is installing.
///
/// The archive and the list are parameters rather than constants for the same
/// reason [`ArchiveSource`] is: the pipeline is the part worth testing, and a
/// pipeline wired directly to the shipped list could only be tested by
/// installing five gigabytes.
pub struct Plan {
    pub source: ArchiveSource,
    /// Oldest tree `pacman -Syuu` can still bring up to `source.version`.
    /// Alongside the source for the same reason: it is part of what this build
    /// ships, and the update matrix is worth testing without a real tree.
    pub min_compatible: String,
    pub list: PackageList,
    pub mounts: fstab::Mounts,
    /// Install every package in the list again, whether or not it is already
    /// there. What `install --force` sets, and what a moved installation asks
    /// for.
    pub force: bool,
}

impl Plan {
    /// What this build of ProxSpace installs.
    pub fn shipped(paths: &Paths) -> Result<Plan, InstallError> {
        Ok(Plan {
            source: ArchiveSource::msys2(),
            min_compatible: msys2::MSYS2_MIN_COMPATIBLE.to_string(),
            list: PackageList::shipped()?,
            mounts: fstab::Mounts::for_paths(paths),
            force: false,
        })
    }

    pub fn forced(mut self, force: bool) -> Plan {
        self.force = force;
        self
    }
}

/// Everything the steps need but never change.
struct Env<'a> {
    runner: &'a dyn CommandRunner,
    ui: &'a Ui,
    paths: &'a Paths,
    pacman: Pacman,
}

impl Env<'_> {
    fn tree(&self) -> PathBuf {
        self.paths.msys2()
    }

    /// A program inside the tree, refusing early if it is not there.
    fn tool(&self, relative: &str) -> Result<PathBuf, InstallError> {
        let path = self.tree().join(relative);
        if path.is_file() {
            Ok(path)
        } else {
            Err(InstallError::ToolMissing { path })
        }
    }

    /// Record a completed step. Written out immediately: the value of the state
    /// file is entirely in it being current when the process dies.
    fn record(&self, state: &mut State, stage: Stage) -> Result<(), InstallError> {
        state.move_to(stage)?;
        state_file::save(state, &self.paths.state_file())?;
        Ok(())
    }
}

/// Bring the environment to [`Stage::Ready`], doing only what is left to do.
///
/// Safe to call before every command: on a finished install it runs
/// `prepare()`, finds every stage already recorded, and returns.
pub fn ensure_ready(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
) -> Result<(), InstallError> {
    msys2::ensure_tree(http, ui, paths, state, &plan.source)?;

    let env = Env {
        runner,
        ui,
        paths,
        pacman: Pacman::new(&paths.msys2()),
    };

    // Every run, not once: it is idempotent, it costs two external calls, and
    // it is what repairs a tree whose `/etc/fstab` or `/etc/passwd` went stale
    // — after a Windows account change, for instance.
    let prepared = msys2::prepare(runner, ui, paths, &plan.mounts)?;
    if prepared.changed_anything() {
        ui.detail("the msys2 tree was brought up to date with this ProxSpace");
    }

    let moved = settle_move(&env, state)?;

    bootstrap(&env, state)?;
    update_core(&env, state)?;
    install_packages(&env, state, &plan.list, plan.force || moved)?;
    finish(&env, state)?;
    Ok(())
}

/// The most characters put on one `pacman -S` command line.
///
/// Windows refuses to start a process whose whole command line runs past 32767
/// characters, and a repair names every package in the tree — several hundred
/// of them once the toolchain and its dependencies are in. The budget leaves
/// room for the path to pacman and its flags.
const MAX_COMMAND_LINE: usize = 24_000;

/// Reinstall every installed package on top of itself.
///
/// This is `ps-repair`, and it exists for a tree whose files are wrong in ways
/// nothing can work out from the outside: a half-written package after a power
/// cut, files an antivirus quarantined, a `.dll` truncated by a full disk.
/// pacman is told to overwrite whatever it finds, so every file in the tree
/// goes back to what the package says it should be.
///
/// Two differences from the original loop of one `pacman -S` per package. The
/// packages go in as few transactions rather than several hundred, which turns
/// half an hour into minutes. And the pinned package is left out of them and
/// reinstalled from its URL instead: named on a `-S` command line it would
/// either be refused because of the pin in `pacman.conf` or, if the pin had
/// gone missing, quietly replaced by a newer version — and a newer
/// `arm-none-eabi-binutils` is exactly the breakage the pin is there to
/// prevent.
pub fn repair(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    plan: &Plan,
    rebase: bool,
) -> Result<(), InstallError> {
    let env = Env {
        runner,
        ui,
        paths,
        pacman: Pacman::new(&paths.msys2()),
    };

    // Cheap, idempotent, and it repairs the breakages that are not about
    // package files at all: an `/etc/fstab` left behind by a moved folder, an
    // account file written for a Windows user that no longer exists.
    let prepared = msys2::prepare(runner, ui, paths, &plan.mounts)?;
    if prepared.changed_anything() {
        ui.info("the msys2 tree was brought up to date with this ProxSpace");
    }

    let installed = env.pacman.query_installed(runner, ui)?;
    let held = env.pacman.ignored().unwrap_or_default();
    let names: Vec<&str> = installed
        .names()
        .filter(|name| !held.iter().any(|pinned| pinned == name))
        .collect();

    if names.is_empty() {
        ui.warn("no packages are installed in this tree; there is nothing to reinstall");
    } else {
        ui.step(&format!("reinstalling {}", describe(&names)));
        let batches = batches(&names);
        for (index, batch) in batches.iter().enumerate() {
            if batches.len() > 1 {
                ui.info(&format!("batch {} of {}", index + 1, batches.len()));
            }
            env.pacman.install(runner, ui, batch, Mode::Reinstall)?;
        }
    }

    let pins: Vec<&PkgSpec> = plan.list.pinned().collect();
    settle_pins(&env, &plan.list, &pins)?;

    if rebase {
        msys2::rebase(runner, ui, paths)?;
    }

    ui.success("the environment has been reinstalled over itself");
    Ok(())
}

/// Split package names into command lines short enough for Windows to start.
fn batches<'a>(names: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut batches: Vec<Vec<&str>> = Vec::new();
    let mut length = 0;

    for name in names {
        // The separating space is what makes this the length of the line
        // rather than of the names.
        let cost = name.len() + 1;
        match batches.last_mut() {
            Some(batch) if length + cost <= MAX_COMMAND_LINE => {
                batch.push(name);
                length += cost;
            }
            _ => {
                batches.push(vec![name]);
                length = cost;
            }
        }
    }
    batches
}

/// Notice that the whole installation has been moved or copied elsewhere, and
/// decide what to do about it.
///
/// msys2 writes absolute paths into the tree as it installs: `/etc/fstab`
/// points at the Windows directories to mount, and a good number of packages
/// bake the prefix they were configured with into scripts, `.pc` files and
/// wrapper binaries. Moving the folder leaves all of those pointing at a
/// directory that is no longer there, and the failures that follow — a build
/// that cannot find its own compiler, a python that imports nothing — say
/// nothing about the cause.
///
/// `/etc/fstab` and the account files are rewritten unconditionally by
/// `prepare()` just before this, so what is left to decide is only whether the
/// packages have to go on top of themselves again.
///
/// Returns whether they do.
fn settle_move(env: &Env<'_>, state: &mut State) -> Result<bool, InstallError> {
    let current = env.paths.base();
    if !state_file::was_moved_from(state, current) {
        return Ok(false);
    }
    let recorded = state.install_path.clone().unwrap_or_default();

    env.ui.warn(&format!(
        "this environment was installed in `{recorded}` and is now in `{}`",
        current.display()
    ));

    // Below this stage nothing has been installed into the tree, so nothing in
    // it can be pointing at the old path.
    let reinstall = if state.stage < Stage::PackagesInstalled {
        false
    } else {
        env.ui.info(
            "packages record the path they were installed under, \
             so they have to be installed again",
        );
        match env.ui.confirm("reinstall the packages now?", true) {
            Ok(answer) => answer,
            // No terminal to ask on and no `--yes`. Spending twenty minutes on
            // a reinstall nobody asked for is the wrong way to resolve that,
            // so say what to run instead and carry on.
            Err(_) => {
                env.ui.warn(
                    "cannot ask whether to reinstall; run `proxspace install --force` \
                     if the environment misbehaves",
                );
                false
            }
        }
    };

    state.install_path = Some(current.to_string_lossy().into_owned());
    state_file::save(state, &env.paths.state_file())?;
    Ok(reinstall)
}

/// Run one login shell and let it exit.
///
/// This is the whole of the first-run bootstrap. msys2 ships its own
/// `/etc/post-install/07-pacman-key.post`, which creates and populates the
/// pacman keyring on the first login and then disables itself; without it every
/// later package install fails on signature verification. Reimplementing
/// `pacman-key --init --populate` here would be duplicating a script that is
/// already in the tree and is upstream's business to keep correct.
fn bootstrap(env: &Env<'_>, state: &mut State) -> Result<(), InstallError> {
    if state.stage >= Stage::Bootstrapped {
        env.ui.detail("msys2 is already initialised");
        return Ok(());
    }
    let bash = env.tool(BASH)?;

    env.ui
        .step("initialising msys2 (the first run generates a keyring and takes a while)");
    env.runner
        .run(
            env.ui,
            &Cmd::new(&bash)
                .envs(msys2::tool_env(&env.tree()))
                .arg("-l")
                .arg("-c")
                .arg("exit")
                .label("initialising msys2"),
        )?
        .check()?;

    env.record(state, Stage::Bootstrapped)?;
    env.ui.success("msys2 initialised");
    Ok(())
}

/// `pacman -Syuu` on a clean set of databases.
fn update_core(env: &Env<'_>, state: &mut State) -> Result<(), InstallError> {
    if state.stage >= Stage::CoreUpdated {
        env.ui.detail("msys2 is already up to date");
        return Ok(());
    }

    env.ui.step("updating msys2");
    run_core_upgrade(env)?;

    env.record(state, Stage::CoreUpdated)?;
    env.ui.success("msys2 updated");
    Ok(())
}

/// `pacman -Syuu` on databases that are thrown away first.
///
/// The databases are reset rather than refreshed because the ones a tree
/// carries are as old as the base archive, and pacman is happier being told to
/// fetch them afresh than to reconcile them.
fn run_core_upgrade(env: &Env<'_>) -> Result<(), InstallError> {
    env.pacman.reset_sync_db(env.ui)?;
    env.pacman.system_upgrade(env.runner, env.ui)?;
    Ok(())
}

/// Why the package step is about to run — or `None` when it need not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// Nothing has been installed into this tree yet.
    Fresh,
    /// The environment is complete, but this build of ProxSpace ships a
    /// different package list than the one it was built from.
    ListChanged,
    /// Asked for outright, or by a folder that has been moved.
    Forced,
}

/// Decide whether the packages need looking at.
///
/// The recorded hash covers the list as it was parsed, so an edit to a comment
/// or a section banner does not count as a change, while a new package, a
/// dropped one or a pin moved to another version all do.
fn reason_to_install(state: &State, list_hash: &str, force: bool) -> Option<Reason> {
    if force {
        return Some(Reason::Forced);
    }
    if state.stage < Stage::PackagesInstalled {
        return Some(Reason::Fresh);
    }
    match &state.packages {
        Some(packages) if packages.list_hash == list_hash => None,
        _ => Some(Reason::ListChanged),
    }
}

/// Install what the list asks for and is not already there.
///
/// A list that has changed since the environment was built adds what it now
/// asks for and leaves everything else alone; there is no reinstall of the
/// sixty packages that did not change. What it does not do is *remove* packages
/// that have been dropped from the list: they may well be what the user's own
/// builds are linking against, and disk space is a poor reason to break them.
fn install_packages(
    env: &Env<'_>,
    state: &mut State,
    list: &PackageList,
    force: bool,
) -> Result<(), InstallError> {
    let list_hash = list.list_hash();
    let Some(reason) = reason_to_install(state, &list_hash, force) else {
        env.ui.detail("the package set is already installed");
        return Ok(());
    };

    match reason {
        Reason::Fresh => env.ui.step("installing packages"),
        Reason::ListChanged => {
            env.ui.step("updating the package set");
            env.ui.info(
                "this ProxSpace expects a different set of packages \
                 than the one this environment was installed with",
            );
        }
        Reason::Forced => env.ui.step("installing every package again"),
    }
    let installed = env.pacman.query_installed(env.runner, env.ui)?;
    let missing = installed.missing(list);

    // Forced, the whole list goes in over whatever is already there; otherwise
    // only what pacman does not report as installed.
    let (from_repositories, mode) = if force {
        (list.repo_names(), Mode::Reinstall)
    } else {
        let names = missing
            .iter()
            .filter(|spec| !spec.is_pinned())
            .map(|spec| spec.name())
            .collect();
        (names, Mode::Needed)
    };
    if from_repositories.is_empty() {
        env.ui.info(match reason {
            // The list changed in a way that needs nothing fetched: a package
            // was dropped from it, or one that was added is already here as
            // somebody else's dependency.
            Reason::ListChanged => "nothing new to install",
            _ => "every repository package is already installed",
        });
    } else {
        env.ui
            .info(&format!("{} to install", describe(&from_repositories)));
        env.pacman
            .install(env.runner, env.ui, &from_repositories, mode)?;
    }

    // Pinned packages: the ones not installed at all, plus the ones an upgrade
    // pulled forward past the pin.
    let mut pins: Vec<&PkgSpec> = if force {
        list.pinned().collect()
    } else {
        missing
            .into_iter()
            .filter(|spec| spec.is_pinned())
            .collect()
    };
    pins.extend(installed.stale_pins(list));
    pins.dedup_by_key(|spec| spec.name());
    settle_pins(env, list, &pins)?;

    state.packages = Some(PackagesInfo {
        installed_at: timestamp(),
        list_hash,
    });
    env.record(state, Stage::PackagesInstalled)?;
    env.ui.success(match reason {
        Reason::ListChanged => "the package set is up to date",
        _ => "packages installed",
    });
    Ok(())
}

/// Install each pinned package from its URL.
fn install_pins(env: &Env<'_>, pins: &[&PkgSpec]) -> Result<(), InstallError> {
    for spec in pins {
        let PkgSpec::Url { url, .. } = spec else {
            continue;
        };
        env.ui.info(&format!("installing {}", spec.describe()));
        env.pacman.install_url(env.runner, env.ui, url)?;
    }
    Ok(())
}

/// Put every pin back where the list says it should be, and check that it
/// stayed there.
///
/// The three steps always go together: installing a pin from its URL means
/// nothing if `pacman.conf` does not then hold it, and holding it means nothing
/// if nobody looks at what is actually installed afterwards. `pins` are the
/// ones that need installing — on a fresh tree that is all of them, after an
/// upgrade only the ones something moved.
fn settle_pins(env: &Env<'_>, list: &PackageList, pins: &[&PkgSpec]) -> Result<(), InstallError> {
    install_pins(env, pins)?;
    write_pin_block(env, list)?;
    verify_pins(env, list)
}

/// Put the list's pins into `pacman.conf`, and nothing else.
///
/// Written after the packages rather than before: `IgnorePkg` applies to `-S`
/// as well, so a pin set in advance would make pacman skip the package when it
/// arrives as a dependency of something else — which on a fresh tree means it
/// never gets installed at all.
fn write_pin_block(env: &Env<'_>, list: &PackageList) -> Result<(), InstallError> {
    let names: Vec<&str> = list.pinned().map(PkgSpec::name).collect();
    if env.pacman.set_ignored(env.ui, &names)? {
        env.ui.detail(&format!(
            "`{}` now holds {}",
            env.pacman.conf_path().display(),
            describe(&names)
        ));
    }
    Ok(())
}

/// Check that every pin ended up at the version it is pinned to.
///
/// `IgnorePkg` can be walked over — by `--overwrite`, by the package arriving
/// as a dependency, or by someone editing `pacman.conf` — and a proxmark3 that
/// fails to build weeks later is a much worse way to find out.
///
/// Packages of the list that are still not installed are only mentioned, not
/// refused: `base-devel` and friends are package *groups*, which pacman
/// installs happily and then does not report from `pacman -Q` under that name.
fn verify_pins(env: &Env<'_>, list: &PackageList) -> Result<(), InstallError> {
    let installed = env.pacman.query_installed(env.runner, env.ui)?;

    if let Some(spec) = installed.stale_pins(list).first() {
        return Err(InstallError::PinNotHeld {
            name: spec.name().to_string(),
            expected: spec.version().unwrap_or("?").to_string(),
            actual: installed.version(spec.name()).unwrap_or("?").to_string(),
        });
    }

    let absent: Vec<&str> = installed
        .missing(list)
        .iter()
        .map(|spec| spec.name())
        .collect();
    if !absent.is_empty() {
        env.ui.detail(&format!(
            "pacman reports no package called {} — expected for the package groups in the list",
            describe(&absent)
        ));
    }
    Ok(())
}

/// The last two things: the python packages, and the cache.
fn finish(env: &Env<'_>, state: &mut State) -> Result<(), InstallError> {
    if !state.pip_extras_installed {
        let python = env.tool(PYTHON)?;
        env.ui
            .step("installing the python packages the client needs");
        env.runner
            .run(
                env.ui,
                &Cmd::new(&python)
                    .envs(msys2::tool_env(&env.tree()))
                    .args(["-m", "pip", "install"])
                    .args(PIP_EXTRAS.iter().copied())
                    // msys2's python is an externally managed environment
                    // (PEP 668), and these two have to go into it: the
                    // proxmark3 client imports them from the interpreter the
                    // environment provides, not from a virtualenv of its own.
                    .arg("--break-system-packages")
                    .label("installing python packages"),
            )?
            .check()?;

        state.pip_extras_installed = true;
        state_file::save(state, &env.paths.state_file())?;
    }

    if state.stage < Stage::Ready {
        // Gigabytes of `.pkg.tar.zst` that will never be needed again. Failing
        // to free them is not a reason to call a finished install unfinished.
        env.ui.step("cleaning up");
        if let Err(error) = env
            .pacman
            .clean_cache(env.runner, env.ui, Cache::Superseded)
        {
            env.ui
                .warn(&format!("the package cache could not be cleaned ({error})"));
        }
        env.record(state, Stage::Ready)?;
    }

    env.ui.success("the environment is ready");
    Ok(())
}

/// "3 packages: a, b, c", shortened once a list stops being readable.
fn describe(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    let head: Vec<&str> = names.iter().take(SHOWN).copied().collect();
    let noun = if names.len() == 1 {
        "package"
    } else {
        "packages"
    };
    if names.len() <= SHOWN {
        format!("{} {noun}: {}", names.len(), head.join(", "))
    } else {
        format!(
            "{} {noun}: {}, and {} more",
            names.len(),
            head.join(", "),
            names.len() - SHOWN
        )
    }
}

/// Whether the environment can be used without running the pipeline again.
/// Cheap: the state file plus one file on disk.
pub fn is_ready(paths: &Paths, state: &State) -> bool {
    state.stage >= Stage::Ready && paths.msys2().join(BASH).is_file()
}

/// Bring an installed tree up to date where it stands: `pacman -Syuu`, then
/// the pins put back.
///
/// This is the non-destructive half of updating. It is also what makes an
/// update worth running at all between msys2 releases: the base archive gets a
/// new datestamp a few times a year, while the packages in it move every week.
///
/// The pins are settled afterwards because an upgrade is exactly the thing that
/// walks over them — `IgnorePkg` can be overruled by a dependency, and a
/// newer `arm-none-eabi-binutils` is what breaks the proxmark3 firmware build.
pub fn upgrade_msys2(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    plan: &Plan,
) -> Result<(), InstallError> {
    let env = Env {
        runner,
        ui,
        paths,
        pacman: Pacman::new(&paths.msys2()),
    };

    ui.step("updating msys2 and everything installed in it");
    run_core_upgrade(&env)?;

    let installed = env.pacman.query_installed(runner, ui)?;
    let moved: Vec<&PkgSpec> = installed.stale_pins(&plan.list);
    if !moved.is_empty() {
        ui.info("the upgrade moved a pinned package; putting it back");
    }
    settle_pins(&env, &plan.list, &moved)?;

    ui.success("msys2 is up to date");
    Ok(())
}

/// Install whatever this build's package list asks for and the tree does not
/// have.
///
/// The other half of updating, and the one that has nothing to do with msys2
/// versions: a newer ProxSpace ships a longer list, and what is missing from it
/// is installed without touching the sixty packages that did not change.
pub fn update_packages(
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
) -> Result<(), InstallError> {
    let env = Env {
        runner,
        ui,
        paths,
        pacman: Pacman::new(&paths.msys2()),
    };
    install_packages(&env, state, &plan.list, plan.force)
}

/// Whether the package set on disk is the one this build ships.
pub fn packages_are_current(state: &State, list: &PackageList) -> bool {
    reason_to_install(state, &list.list_hash(), false).is_none()
}

/// Throw the msys2 tree away and install it again from scratch.
///
/// The last resort of the update matrix, for a tree too old for `pacman -Syuu`
/// to bring all the way forward. It costs a download and a full package install
/// — twenty minutes and five gigabytes — which is why nothing reaches here
/// without the user having agreed to it.
///
/// Only `msys2/` goes. `pm3/` and `builds/` are never touched by this or by
/// anything else in ProxSpace: the proxmark3 sources and everything built from
/// them are the user's, and a version bump is no reason to take months of work
/// with it.
pub fn reinstall_msys2(
    http: &dyn HttpClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
) -> Result<(), InstallError> {
    let tree = paths.msys2();

    // Windows will not delete a folder anything is running from, and the
    // leftovers pacman keeps around — gpg-agent and friends — are exactly that.
    if procs::stop_holders(&tree, ui)?.stopped_anything() {
        ui.detail("everything using the tree has been stopped");
    }

    ui.step("removing the old msys2 tree");
    archive::remove_tree(&tree)?;

    // Written out before the install starts: a run that dies between the
    // removal and the first step of the install must not leave a state file
    // describing a tree that is no longer there.
    state.forget_msys2()?;
    state_file::save(state, &paths.state_file())?;
    ui.detail("the old tree is gone; `pm3` and `builds` were left alone");

    ensure_ready(http, runner, ui, paths, state, plan)
}

/// Decide what an update run does, from the state file and what is on disk.
pub fn plan_update(paths: &Paths, state: &State, plan: &Plan, reinstall: Reinstall) -> Update {
    // Both halves have to agree that there is a tree: a state file left behind
    // by a deleted folder describes nothing, and a folder no state file knows
    // about cannot be told apart from a half-finished install.
    let installed = match (&state.msys2, paths.msys2().join(BASH).is_file()) {
        (Some(info), true) => Some(info.version.as_str()),
        _ => None,
    };
    decide_update(
        installed,
        &plan.source.version,
        &plan.min_compatible,
        reinstall,
    )
}

/// Show the plan, and get it agreed to when it destroys the tree.
///
/// Returns whether to go ahead. The plan is always shown before anything
/// happens: an update that turns out to mean "your five gigabytes are about to
/// be deleted" should never be a surprise.
pub fn confirm_update(ui: &Ui, update: &Update) -> bool {
    match update {
        Update::Newer { .. } | Update::Blocked { .. } => ui.warn(&update.summary()),
        _ => ui.info(&update.summary()),
    }

    if update.is_blocked() {
        return false;
    }
    if !update.destroys_the_tree() {
        return true;
    }

    ui.info("`pm3` and `builds` are not touched; the proxmark3 sources and anything built from them stay");
    match ui.confirm("delete the msys2 tree and install it again?", false) {
        Ok(answer) => {
            if !answer {
                ui.info("left as it is; the environment goes on working as before");
            }
            answer
        }
        // No terminal to ask on and no `--yes`. Deleting gigabytes on a guess
        // is the one thing that must not happen here.
        Err(_) => {
            ui.warn(
                "cannot ask whether to reinstall msys2; run the command again with `--yes` \
                 to agree to it in advance",
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command line one batch would become, near enough: the names and the
    /// spaces between them.
    fn line_length(batch: &[&str]) -> usize {
        batch.iter().map(|name| name.len() + 1).sum()
    }

    #[test]
    fn the_flags_translate_into_the_override() {
        assert_eq!(Reinstall::from_flags(false, false), Reinstall::WhenNeeded);
        assert_eq!(Reinstall::from_flags(true, false), Reinstall::Always);
        assert_eq!(Reinstall::from_flags(false, true), Reinstall::Never);
        assert_eq!(Reinstall::default(), Reinstall::WhenNeeded);
    }

    #[test]
    fn a_short_list_is_one_command_line() {
        let names = ["git", "make", "python"];
        assert_eq!(batches(&names), vec![vec!["git", "make", "python"]]);
        assert!(batches(&[]).is_empty());
    }

    /// A finished tree holds several hundred packages, and naming them all at
    /// once would build a command line Windows refuses to start.
    #[test]
    fn a_whole_tree_is_split_into_command_lines_windows_will_start() {
        let owned: Vec<String> = (0..2000)
            .map(|number| format!("mingw-w64-ucrt-x86_64-package-{number:04}"))
            .collect();
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();

        let batches = batches(&names);

        assert!(batches.len() > 1, "2000 packages must not be one line");
        for batch in &batches {
            assert!(!batch.is_empty());
            assert!(line_length(batch) <= MAX_COMMAND_LINE);
        }
        // Splitting must not lose or reorder anything.
        let rejoined: Vec<&str> = batches.concat();
        assert_eq!(rejoined, names);
    }

    /// A finished installation built from `hash`.
    fn ready_with(hash: &str) -> State {
        State {
            stage: Stage::Ready,
            packages: Some(PackagesInfo {
                installed_at: timestamp(),
                list_hash: hash.to_string(),
            }),
            ..State::default()
        }
    }

    #[test]
    fn a_finished_installation_of_the_same_list_needs_nothing() {
        assert_eq!(reason_to_install(&ready_with("abc"), "abc", false), None);
    }

    #[test]
    fn an_unfinished_installation_installs_the_packages() {
        for stage in [
            Stage::NotInstalled,
            Stage::Extracted,
            Stage::Bootstrapped,
            Stage::CoreUpdated,
        ] {
            let state = State {
                stage,
                ..State::default()
            };
            assert_eq!(
                reason_to_install(&state, "abc", false),
                Some(Reason::Fresh),
                "at {stage}"
            );
        }
    }

    #[test]
    fn a_different_list_is_taken_up_without_being_forced() {
        assert_eq!(
            reason_to_install(&ready_with("abc"), "def", false),
            Some(Reason::ListChanged)
        );

        // The same, in the shape a state file written before the packages step
        // ever ran would have: the stage says done, the record does not.
        let state = State {
            stage: Stage::Ready,
            packages: None,
            ..State::default()
        };
        assert_eq!(
            reason_to_install(&state, "abc", false),
            Some(Reason::ListChanged)
        );
    }

    #[test]
    fn forcing_beats_every_reason_to_do_nothing() {
        assert_eq!(
            reason_to_install(&ready_with("abc"), "abc", true),
            Some(Reason::Forced)
        );
    }

    #[test]
    fn the_shipped_list_matches_itself_across_two_runs() {
        // What keeps an ordinary start from touching pacman at all: the hash
        // of the same list has to come out the same every time.
        let first = PackageList::shipped().unwrap().list_hash();
        let second = PackageList::shipped().unwrap().list_hash();
        assert_eq!(first, second);
        assert_eq!(reason_to_install(&ready_with(&first), &second, false), None);
    }

    #[test]
    fn lists_of_packages_read_as_sentences() {
        assert_eq!(describe(&["git"]), "1 package: git");
        assert_eq!(describe(&["git", "make"]), "2 packages: git, make");

        let many: Vec<String> = (1..=10).map(|n| format!("p{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            describe(&many),
            "10 packages: p1, p2, p3, p4, p5, p6, p7, p8, and 2 more"
        );
    }
}
