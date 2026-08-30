//! The install pipeline: which external commands run, in what order, and what
//! the state file says when one of them fails.
//!
//! Nothing here talks to a mirror or to a real msys2. The tree is a handful of
//! empty files with the right names, and pacman is a [`CommandRunner`] that
//! remembers what it was asked to do and keeps a little map of what is
//! "installed" so that a second run can see the effect of the first. That map
//! is the whole point: without it "the second run does nothing" and "only the
//! new package is installed" are not testable at all, and those are exactly the
//! behaviours the original got wrong.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use proxspace::app::install::{self, InstallError, Plan};
use proxspace::app::repair;
use proxspace::app::update::{self, Options, Outcome, UpdateError};
use proxspace::core::packages::{PackageList, split_package_file};
use proxspace::core::pacman::MANAGED_BEGIN;
use proxspace::core::paths::Paths;
use proxspace::core::state::{Msys2Info, Stage, State};
use proxspace::core::update::Reinstall;
use proxspace::infra::msys2::{ArchiveSource, fstab::Mounts};
use proxspace::infra::pacman;
use proxspace::infra::state as state_file;
use proxspace::ports::command::{Cmd, CommandError, CommandRunner, Output};
use proxspace::ports::http::{HttpClient, HttpError, Request, Response};
use proxspace::ui::logging::Logger;
use proxspace::ui::{Ui, UiOptions};

/// Real `mkpasswd -c` / `mkgroup -c` output, with the account renamed.
const MKPASSWD: &str = "somebody:unused:1197603:1049089:\
    U-DESKTOP\\somebody,S-1-5-21-1234567890-987654321-1122334455-1001:/home/somebody:/bin/bash\n";
const MKGROUP: &str = "Domain Users:S-1-5-21-1234567890-987654321-1122334455-513:1049089:\n";

const PIN_URL: &str = "https://repo.msys2.org/mingw/ucrt64/\
     mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.46.1-1-any.pkg.tar.zst";
const PIN_NAME: &str = "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils";
const PIN_FILE: &str = "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.46.1-1-any.pkg.tar.zst";

/// The list these tests install: two ordinary packages and the pin.
fn list() -> PackageList {
    PackageList::parse(&format!("git\nmake\n{PIN_URL}\n")).unwrap()
}

fn ui(assume_yes: bool) -> Ui {
    Ui::new(
        UiOptions {
            quiet: true,
            assume_yes,
            ..UiOptions::default()
        },
        Arc::new(Logger::disabled()),
    )
}

/// A network that must not be used: everything here starts from a tree that is
/// already unpacked, so a request would mean the pipeline lost its place.
struct NoNetwork;

impl HttpClient for NoNetwork {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        panic!("nothing should be downloaded, but `{}` was", request.url);
    }
}

/// A network that is there but answers nothing. Used where the reinstall gets
/// as far as needing the archive: what happens after the old tree is gone is
/// the install pipeline, which the rest of this file already covers.
struct DeadNetwork;

impl HttpClient for DeadNetwork {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        Err(HttpError::Transport {
            url: request.url.clone(),
            source: std::io::Error::other("the network is down"),
        })
    }
}

/// pacman, bash and python, as far as the pipeline can tell.
#[derive(Default)]
struct Fake {
    /// Every command run, reduced to its interesting parts.
    calls: Mutex<Vec<String>>,
    /// What `pacman -Q` reports, grown by every successful install.
    installed: Mutex<BTreeMap<String, String>>,
    /// Command-line fragment that makes the next matching call fail.
    fails_on: Mutex<Option<String>>,
    /// Version `pacman -U` records, when it is not the one in the file name.
    forces_pin_version: Mutex<Option<String>>,
}

impl Fake {
    fn with_installed(packages: &[(&str, &str)]) -> Fake {
        let fake = Fake::default();
        for (name, version) in packages {
            fake.installed
                .lock()
                .unwrap()
                .insert((*name).to_string(), (*version).to_string());
        }
        fake
    }

    fn failing_on(self, fragment: &str) -> Fake {
        *self.fails_on.lock().unwrap() = Some(fragment.to_string());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Drop the record of what has run so far, so that a test can set an
    /// environment up with this same fake and then watch only what follows.
    fn forget(&self) {
        self.calls.lock().unwrap().clear();
    }

    fn ran(&self, fragment: &str) -> bool {
        self.calls().iter().any(|call| call.contains(fragment))
    }

    fn version_of(&self, name: &str) -> Option<String> {
        self.installed.lock().unwrap().get(name).cloned()
    }

    /// Apply what a pacman command would have done to the installed set.
    fn apply(&self, args: &[String]) {
        let operands: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
        let mut installed = self.installed.lock().unwrap();

        if args.iter().any(|arg| arg == "-S") {
            for name in operands {
                installed.insert(name.clone(), "1.0-1".to_string());
            }
        } else if args.iter().any(|arg| arg == "-U") {
            for url in operands {
                let file = url.rsplit('/').next().unwrap_or(url);
                let (name, version) = split_package_file(file).expect("a package URL");
                let version = self
                    .forces_pin_version
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(version);
                installed.insert(name, version);
            }
        }
    }
}

impl CommandRunner for Fake {
    fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
        let program = cmd
            .program
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let call = summarise(&program, &args);
        self.calls.lock().unwrap().push(call.clone());

        if let Some(fragment) = self.fails_on.lock().unwrap().as_ref()
            && call.contains(fragment.as_str())
        {
            return Ok(Output::new(
                Some(1),
                "",
                "error: failed retrieving file 'ucrt64.db' from mirror.msys2.org",
                cmd.describe(),
            ));
        }

        let stdout = match program.as_str() {
            "mkpasswd" => MKPASSWD.to_string(),
            "mkgroup" => MKGROUP.to_string(),
            "pacman" if args.iter().any(|arg| arg == "-Q") => self
                .installed
                .lock()
                .unwrap()
                .iter()
                .map(|(name, version)| format!("{name} {version}\n"))
                .collect(),
            "pacman" => {
                self.apply(&args);
                String::new()
            }
            _ => String::new(),
        };
        Ok(Output::new(Some(0), stdout, "", cmd.describe()))
    }
}

/// One command as a short, comparable line: the program, and the arguments that
/// say what it was for. The constant flags carry no information and would only
/// make the expected sequences unreadable.
fn summarise(program: &str, args: &[String]) -> String {
    let interesting: Vec<String> = args
        .iter()
        .filter(|arg| {
            !matches!(
                arg.as_str(),
                "--noconfirm" | "--overwrite=*" | "--break-system-packages"
            )
        })
        // A package URL is identified well enough by its file name.
        .map(|arg| match arg.rsplit_once('/') {
            Some((_, file)) if arg.starts_with("http") => file.to_string(),
            _ => arg.clone(),
        })
        .collect();
    format!("{program} {}", interesting.join(" "))
        .trim_end()
        .to_string()
}

/// An installation with the msys2 tree unpacked and nothing done to it yet.
fn unpacked() -> (tempfile::TempDir, Paths, State) {
    let dir = tempfile::tempdir().unwrap();
    let paths = proxspace::infra::paths::from_dir(dir.path()).unwrap();
    let tree = paths.msys2();

    fs::create_dir_all(tree.join("usr/bin")).unwrap();
    fs::create_dir_all(tree.join("ucrt64/bin")).unwrap();
    fs::create_dir_all(tree.join("etc")).unwrap();
    fs::create_dir_all(tree.join("var/lib/pacman/sync")).unwrap();
    for tool in [
        "usr/bin/mkpasswd.exe",
        "usr/bin/mkgroup.exe",
        "usr/bin/pacman.exe",
        "usr/bin/bash.exe",
        "ucrt64/bin/python.exe",
    ] {
        fs::write(tree.join(tool), b"not really a program").unwrap();
    }
    fs::copy("tests/fixtures/pacman.conf", tree.join(pacman::CONF_PATH)).unwrap();
    // A database left over from the base archive, to be thrown away before the
    // first upgrade.
    fs::write(tree.join("var/lib/pacman/sync/ucrt64.db"), b"stale").unwrap();

    let state = State {
        stage: Stage::Extracted,
        install_path: Some(paths.base().to_string_lossy().into_owned()),
        ..State::default()
    };
    (dir, paths, state)
}

fn plan(paths: &Paths, force: bool) -> Plan {
    Plan {
        source: ArchiveSource::msys2(),
        min_compatible: proxspace::infra::msys2::MSYS2_MIN_COMPATIBLE.to_string(),
        list: list(),
        mounts: Mounts::for_paths(paths),
        force,
    }
}

fn run(fake: &Fake, paths: &Paths, state: &mut State, force: bool) -> Result<(), InstallError> {
    install::ensure_ready(
        &NoNetwork,
        fake,
        &ui(true),
        paths,
        state,
        &plan(paths, force),
    )
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn a_fresh_environment_runs_every_step_in_order() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();

    run(&fake, &paths, &mut state, false).unwrap();

    let expected: Vec<String> = [
        // Who the user is, so `/etc/passwd` can be written.
        "mkpasswd -c".to_string(),
        "mkgroup -c".to_string(),
        // One login, which is what initialises the pacman keyring.
        "bash -l -c exit".to_string(),
        // The runtime itself, before anything is installed on top of it.
        "pacman -Syuu".to_string(),
        // What is already there, then the difference.
        "pacman -Q".to_string(),
        "pacman -S --needed git make".to_string(),
        format!("pacman -U {PIN_FILE}"),
        // The pin is checked rather than assumed.
        "pacman -Q".to_string(),
        "python -m pip install ansicolors sslcrypto".to_string(),
        "pacman -Sc".to_string(),
    ]
    .into();
    assert_eq!(fake.calls(), expected);

    assert_eq!(state.stage, Stage::Ready);
    assert!(state.pip_extras_installed);
    assert_eq!(
        state.packages.as_ref().unwrap().list_hash,
        list().list_hash()
    );
}

#[test]
fn the_stale_database_is_thrown_away_before_the_upgrade() {
    let (_dir, paths, mut state) = unpacked();
    let database = paths.msys2().join("var/lib/pacman/sync/ucrt64.db");

    run(&Fake::default(), &paths, &mut state, false).unwrap();

    assert!(!database.exists(), "the shipped database must not survive");
    assert!(database.parent().unwrap().is_dir());
}

#[test]
fn the_pin_ends_up_in_pacman_conf_and_at_the_pinned_version() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();

    run(&fake, &paths, &mut state, false).unwrap();

    let conf = read(&paths.msys2().join(pacman::CONF_PATH));
    assert!(conf.contains(MANAGED_BEGIN));
    assert!(conf.contains(&format!("IgnorePkg = {PIN_NAME}")));
    // Inside `[options]`, where pacman actually reads it.
    assert!(conf.find(MANAGED_BEGIN) < conf.find("\n[clangarm64]"));

    assert_eq!(fake.version_of(PIN_NAME).as_deref(), Some("2.46.1-1"));
}

#[test]
fn a_second_run_asks_pacman_nothing() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();

    let again =
        Fake::with_installed(&[("git", "1.0-1"), ("make", "1.0-1"), (PIN_NAME, "2.46.1-1")]);
    run(&again, &paths, &mut state, false).unwrap();

    // The account is still asked for — that is what keeps `/etc/passwd` right
    // after a Windows account change — but nothing is installed or queried.
    assert_eq!(again.calls(), vec!["mkpasswd -c", "mkgroup -c"]);
    assert_eq!(state.stage, Stage::Ready);
}

#[test]
fn each_stage_is_resumed_from_rather_than_repeated() {
    let cases = [
        (Stage::Extracted, vec!["bash -l -c exit", "pacman -Syuu"]),
        (Stage::Bootstrapped, vec!["pacman -Syuu"]),
        (Stage::CoreUpdated, vec!["pacman -S --needed git make"]),
    ];

    for (stage, expected) in cases {
        let (_dir, paths, mut state) = unpacked();
        state.stage = stage;
        let fake = Fake::default();

        run(&fake, &paths, &mut state, false).unwrap();

        for wanted in &expected {
            assert!(fake.ran(wanted), "at {stage}: `{wanted}` did not run");
        }
        if stage >= Stage::Bootstrapped {
            assert!(
                !fake.ran("bash"),
                "at {stage}: the tree was bootstrapped again"
            );
        }
        if stage >= Stage::CoreUpdated {
            assert!(!fake.ran("-Syuu"), "at {stage}: msys2 was updated again");
        }
        assert_eq!(state.stage, Stage::Ready);
    }
}

#[test]
fn a_failure_leaves_the_stage_at_the_last_step_that_finished() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default().failing_on("-Syuu");

    let error = run(&fake, &paths, &mut state, false).unwrap_err();

    assert!(matches!(error, InstallError::Pacman(_)));
    assert!(
        error
            .to_string()
            .contains("the mirrors could not be reached")
    );
    // The login shell did run, so that much is recorded; the upgrade did not.
    assert_eq!(state.stage, Stage::Bootstrapped);
    assert!(state.packages.is_none());
    assert!(!state.pip_extras_installed);
}

#[test]
fn the_run_after_a_failure_continues_where_it_stopped() {
    let (_dir, paths, mut state) = unpacked();
    run(
        &Fake::default().failing_on("-Syuu"),
        &paths,
        &mut state,
        false,
    )
    .unwrap_err();

    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();

    assert!(!fake.ran("bash"), "the keyring was generated a second time");
    assert!(fake.ran("-Syuu"));
    assert_eq!(state.stage, Stage::Ready);
}

#[test]
fn a_package_added_to_the_list_is_the_only_one_installed() {
    let (_dir, paths, mut state) = unpacked();
    run(&Fake::default(), &paths, &mut state, false).unwrap();

    let longer = PackageList::parse(&format!("git\nmake\npkgconf\n{PIN_URL}\n")).unwrap();
    let fake = Fake::with_installed(&[("git", "1.0-1"), ("make", "1.0-1"), (PIN_NAME, "2.46.1-1")]);
    install::ensure_ready(
        &NoNetwork,
        &fake,
        &ui(true),
        &paths,
        &mut state,
        &Plan {
            list: longer.clone(),
            ..plan(&paths, false)
        },
    )
    .unwrap();

    assert!(fake.ran("pacman -S --needed pkgconf"));
    assert!(!fake.ran("git"), "an installed package was installed again");
    // The pin is already at its version, so it is not fetched again either.
    assert!(!fake.ran("-U"));
    assert_eq!(
        state.packages.as_ref().unwrap().list_hash,
        longer.list_hash()
    );
}

#[test]
fn a_pin_moved_to_another_version_is_installed_again() {
    let (_dir, paths, mut state) = unpacked();
    run(&Fake::default(), &paths, &mut state, false).unwrap();

    let newer_url = PIN_URL.replace("2.46.1-1", "2.47.0-1");
    let newer = PackageList::parse(&format!("git\nmake\n{newer_url}\n")).unwrap();
    let fake = Fake::with_installed(&[("git", "1.0-1"), ("make", "1.0-1"), (PIN_NAME, "2.46.1-1")]);

    install::ensure_ready(
        &NoNetwork,
        &fake,
        &ui(true),
        &paths,
        &mut state,
        &Plan {
            list: newer,
            ..plan(&paths, false)
        },
    )
    .unwrap();

    assert!(fake.ran("-U mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.47.0-1"));
    assert_eq!(fake.version_of(PIN_NAME).as_deref(), Some("2.47.0-1"));
}

#[test]
fn a_pin_that_will_not_stay_pinned_is_an_error_rather_than_a_broken_build() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    // An upgrade that walks over `IgnorePkg` and leaves the repository version
    // behind: the case the check after the install exists for.
    *fake.forces_pin_version.lock().unwrap() = Some("2.99.0-1".to_string());

    let error = run(&fake, &paths, &mut state, false).unwrap_err();

    match error {
        InstallError::PinNotHeld {
            name,
            expected,
            actual,
        } => {
            assert_eq!(name, PIN_NAME);
            assert_eq!(expected, "2.46.1-1");
            assert_eq!(actual, "2.99.0-1");
        }
        other => panic!("expected a held-back pin, got: {other}"),
    }
    assert_ne!(state.stage, Stage::Ready);
}

#[test]
fn forcing_installs_the_whole_list_over_itself() {
    let (_dir, paths, mut state) = unpacked();
    run(&Fake::default(), &paths, &mut state, false).unwrap();

    let fake = Fake::with_installed(&[("git", "1.0-1"), ("make", "1.0-1"), (PIN_NAME, "2.46.1-1")]);
    run(&fake, &paths, &mut state, true).unwrap();

    // Everything, and without `--needed`, which would skip it all.
    assert!(fake.ran("pacman -S git make"));
    assert!(!fake.ran("--needed"));
    assert!(fake.ran(&format!("-U {PIN_FILE}")));
    assert_eq!(state.stage, Stage::Ready);
}

#[test]
fn a_moved_installation_installs_its_packages_again() {
    let (_dir, paths, mut state) = unpacked();
    run(&Fake::default(), &paths, &mut state, false).unwrap();

    // The folder the environment was built in is gone; this is a copy of it.
    state.install_path = Some(r"D:\somewhere\else\ProxSpace".to_string());

    let fake = Fake::with_installed(&[("git", "1.0-1"), ("make", "1.0-1"), (PIN_NAME, "2.46.1-1")]);
    run(&fake, &paths, &mut state, false).unwrap();

    assert!(fake.ran("pacman -S git make"));
    // And the new location is what the state file records from now on.
    assert_eq!(
        state.install_path.as_deref(),
        Some(paths.base().to_string_lossy().as_ref())
    );
}

#[test]
fn a_moved_installation_that_was_never_finished_installs_nothing_twice() {
    let (_dir, paths, mut state) = unpacked();
    state.install_path = Some(r"D:\somewhere\else\ProxSpace".to_string());
    let fake = Fake::default();

    run(&fake, &paths, &mut state, false).unwrap();

    // Nothing had been installed into the tree, so there was nothing that could
    // be pointing at the old path: the normal first install, with `--needed`.
    assert!(fake.ran("pacman -S --needed git make"));
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|call| call.contains("-S "))
            .count(),
        1
    );
}

#[test]
fn an_incomplete_tree_says_which_program_is_missing() {
    let (_dir, paths, mut state) = unpacked();
    fs::remove_file(paths.msys2().join("ucrt64/bin/python.exe")).unwrap();

    let error = run(&Fake::default(), &paths, &mut state, false).unwrap_err();

    assert!(matches!(error, InstallError::ToolMissing { .. }));
    assert!(error.to_string().contains("python.exe"));
    // Everything before the python step still counts as done.
    assert_eq!(state.stage, Stage::PackagesInstalled);
}

#[test]
fn a_repair_puts_every_installed_package_back_over_itself() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();
    let before = fake.calls().len();

    repair::repair(&fake, &ui(true), &paths, &plan(&paths, false), false).unwrap();

    let calls = fake.calls();
    let during = &calls[before..];
    assert!(
        during.iter().any(|call| call == "pacman -S git make"),
        "got: {during:?}"
    );
    // The pin is never named on a `-S` line: pacman would either refuse it
    // because of `IgnorePkg` or hand back a newer version than the pin.
    assert!(
        !during
            .iter()
            .any(|call| call.starts_with("pacman -S") && call.contains(PIN_NAME)),
        "got: {during:?}"
    );
    // It comes back from its URL instead, at the version the list demands.
    assert!(
        during
            .iter()
            .any(|call| call == &format!("pacman -U {PIN_FILE}")),
        "got: {during:?}"
    );
    assert_eq!(fake.version_of(PIN_NAME).as_deref(), Some("2.46.1-1"));
}

#[test]
fn a_repair_of_an_empty_tree_asks_pacman_to_install_nothing() {
    let (_dir, paths, _state) = unpacked();
    let fake = Fake::default();

    repair::repair(&fake, &ui(true), &paths, &plan(&paths, false), false).unwrap();

    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| call.starts_with("pacman -S ")),
        "got: {:?}",
        fake.calls()
    );
}

/// Rebasing takes minutes and is the reason it was taken out of every start; it
/// must happen when it is asked for and never otherwise.
#[test]
fn the_dlls_are_rebased_only_when_asked() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();
    fs::write(
        paths.msys2().join("usr/bin/dash.exe"),
        b"not really a program",
    )
    .unwrap();

    repair::repair(&fake, &ui(true), &paths, &plan(&paths, false), false).unwrap();
    assert!(!fake.ran("rebaseall"));

    repair::repair(&fake, &ui(true), &paths, &plan(&paths, false), true).unwrap();
    assert!(fake.ran("rebaseall"), "got: {:?}", fake.calls());
}

/// The reinstall exists to replace a tree too old to be upgraded, and the whole
/// worry about it is what it takes with it. Nothing outside `msys2` may go, and
/// the state must not go on describing a tree that has been deleted.
#[test]
fn a_full_reinstall_takes_the_tree_and_leaves_the_users_work() {
    let (_dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();
    assert_eq!(state.stage, Stage::Ready);

    // Months of somebody's work, sitting where ProxSpace puts it.
    for directory in [paths.pm3(), paths.builds()] {
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("keep-me.c"), b"int main(void) { return 0; }").unwrap();
    }

    let error = install::reinstall_msys2(
        &DeadNetwork,
        &fake,
        &ui(true),
        &paths,
        &mut state,
        &plan(&paths, false),
    )
    .unwrap_err();

    // The old tree is gone; the new one could not be downloaded.
    assert!(!paths.msys2().exists(), "the old tree survived: {error}");
    for directory in [paths.pm3(), paths.builds()] {
        assert!(
            directory.join("keep-me.c").is_file(),
            "`{}` lost the user's files",
            directory.display()
        );
    }

    // Nothing left claiming an install that is not there.
    let saved = state_file::load(&paths.state_file()).state;
    assert_eq!(saved.stage, Stage::NotInstalled);
    assert_eq!(saved.msys2, None);
    assert_eq!(saved.packages, None);
    assert!(!saved.pip_extras_installed);
}

// --- `update`: the two halves, and what each of them runs ---

/// A finished environment whose state records the given msys2 version.
fn updatable(version: &str) -> (tempfile::TempDir, Paths, State, Fake) {
    let (dir, paths, mut state) = unpacked();
    let fake = Fake::default();
    run(&fake, &paths, &mut state, false).unwrap();
    state.msys2 = Some(Msys2Info {
        version: version.to_string(),
        source_url: "https://mirror.test/msys2-base-x86_64.tar.xz".to_string(),
        sha256: "0".repeat(64),
        extracted_at: "2026-08-27T10:00:00Z".to_string(),
    });
    state_file::save(&state, &paths.state_file()).unwrap();
    fake.forget();
    (dir, paths, state, fake)
}

fn update_plan(paths: &Paths, shipped: &str, min_compatible: &str) -> Plan {
    let mut plan = plan(paths, false);
    plan.source.version = shipped.to_string();
    plan.min_compatible = min_compatible.to_string();
    plan
}

fn options(msys2: bool, packages: bool, check: bool, reinstall: Reinstall) -> Options {
    Options {
        msys2,
        packages,
        check,
        reinstall,
    }
}

fn update(
    fake: &Fake,
    paths: &Paths,
    state: &mut State,
    plan: &Plan,
    options: &Options,
) -> Result<Outcome, UpdateError> {
    update::run(&DeadNetwork, fake, &ui(true), paths, state, plan, options)
}

/// The base archive gets a new datestamp a few times a year; the packages in it
/// move every week. An update of a tree that is already at the shipped version
/// still has work to do.
#[test]
fn updating_a_current_tree_still_upgrades_what_is_installed_in_it() {
    let (_dir, paths, mut state, fake) = updatable("20260611");

    let outcome = update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(true, false, false, Reinstall::WhenNeeded),
    )
    .unwrap();

    assert_eq!(outcome, Outcome::Done);
    assert!(fake.ran("-Syuu"), "no upgrade ran: {:?}", fake.calls());
}

#[test]
fn an_upgrade_records_the_version_it_reached() {
    let (_dir, paths, mut state, fake) = updatable("20260101");

    update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(true, false, false, Reinstall::WhenNeeded),
    )
    .unwrap();

    assert!(fake.ran("-Syuu"));
    let saved = state_file::load(&paths.state_file()).state;
    assert_eq!(saved.msys2.expect("msys2 info").version, "20260611");
}

/// A binary rolled back to an older ProxSpace: the tree keeps the version it
/// has, because there is no downgrading msys2, but it is still brought forward.
#[test]
fn a_tree_newer_than_the_binary_keeps_its_recorded_version() {
    let (_dir, paths, mut state, fake) = updatable("20270101");

    update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(true, false, false, Reinstall::WhenNeeded),
    )
    .unwrap();

    assert!(fake.ran("-Syuu"));
    let saved = state_file::load(&paths.state_file()).state;
    assert_eq!(saved.msys2.expect("msys2 info").version, "20270101");
}

#[test]
fn updating_only_the_packages_leaves_msys2_alone() {
    let (_dir, paths, mut state, fake) = updatable("20260101");

    update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(false, true, false, Reinstall::WhenNeeded),
    )
    .unwrap();

    assert!(!fake.ran("-Syuu"), "msys2 was updated: {:?}", fake.calls());
    let saved = state_file::load(&paths.state_file()).state;
    assert_eq!(saved.msys2.expect("msys2 info").version, "20260101");
}

/// `--check` is for finding out what a run would do, so it must be safe to
/// type on any environment: no external command, no write. The one thing it
/// does reach for is GitHub, to say whether a newer ProxSpace exists, and that
/// request failing changes nothing.
#[test]
fn checking_runs_nothing_and_changes_nothing() {
    let (_dir, paths, mut state, fake) = updatable("20240101");
    let before = fs::read_to_string(paths.state_file()).unwrap();

    let outcome = update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(false, false, true, Reinstall::WhenNeeded),
    )
    .unwrap();

    assert_eq!(outcome, Outcome::Checked);
    assert_eq!(fake.calls(), Vec::<String>::new());
    assert_eq!(fs::read_to_string(paths.state_file()).unwrap(), before);
    assert!(paths.msys2().is_dir());
}

/// The point of `--no-reinstall`: a tree too old to upgrade is reported, not
/// deleted.
#[test]
fn refusing_a_reinstall_is_an_error_and_not_a_deleted_tree() {
    let (_dir, paths, mut state, fake) = updatable("20240101");

    let error = update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(true, false, false, Reinstall::Never),
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("--no-reinstall"),
        "unexpected error: {error}"
    );
    assert!(paths.msys2().is_dir(), "the tree was removed anyway");
    assert_eq!(fake.calls(), Vec::<String>::new());
}

/// Asking for a reinstall of a tree that needs nothing: the versions are not
/// consulted, and what goes is the tree, not the user's work.
#[test]
fn asking_for_a_reinstall_replaces_even_a_current_tree() {
    let (_dir, paths, mut state, fake) = updatable("20260611");

    update(
        &fake,
        &paths,
        &mut state,
        &update_plan(&paths, "20260611", "20250101"),
        &options(true, false, false, Reinstall::Always),
    )
    .unwrap_err();

    // The download is what fails; by then the old tree is already gone.
    assert!(!paths.msys2().exists());
    let saved = state_file::load(&paths.state_file()).state;
    assert_eq!(saved.stage, Stage::NotInstalled);
}
