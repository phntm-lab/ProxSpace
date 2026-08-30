//! Putting every installed package back over itself.
//!
//! Not part of the install automaton, and deliberately so: the tree is already
//! there and wrong, so the pipeline that works out what is missing is exactly
//! the wrong tool. This asks the tree what it holds and installs all of it
//! again.

use crate::app::install::{Env, InstallError, Plan, batches, describe, settle_pins};
use crate::core::packages::PkgSpec;
use crate::core::paths::Paths;
use crate::infra::msys2;
use crate::infra::pacman::{Mode, Pacman};
use crate::ports::command::CommandRunner;
use crate::ui::Ui;

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
