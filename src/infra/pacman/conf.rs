//! The `IgnorePkg` block ProxSpace owns in `pacman.conf`.
//!
//! What the file should look like is worked out in [`crate::core::pacman`];
//! this reads it, writes it back safely, and turns a malformed block into an
//! error that names the file.

use std::fs;
use std::path::Path;

use crate::core::pacman::{IgnoreBlockError, managed_ignores, with_ignores};
use crate::infra::pacman::PacmanError;
use crate::ui::Ui;

/// Give a block problem the path it happened in.
fn block_error(error: IgnoreBlockError, conf: &Path) -> PacmanError {
    match error {
        IgnoreBlockError::Broken => PacmanError::BrokenBlock {
            path: conf.to_path_buf(),
        },
        IgnoreBlockError::NoOptionsSection => PacmanError::NoOptionsSection {
            path: conf.to_path_buf(),
        },
    }
}

/// Write, update or remove the `IgnorePkg` block in `pacman.conf`.
///
/// Returns whether the file changed, so that a run which changes nothing
/// says nothing. An empty list removes the block entirely — that is how a
/// pin that has been dropped from the package list stops being a pin.
pub fn set_ignored(conf: &Path, ui: &Ui, names: &[&str]) -> Result<bool, PacmanError> {
    let current = fs::read_to_string(conf).map_err(|source| PacmanError::Io {
        action: "read",
        path: conf.to_path_buf(),
        source,
    })?;
    let updated = with_ignores(&current, names).map_err(|error| block_error(error, conf))?;
    if updated == current {
        return Ok(false);
    }

    // Written through a temporary file: a half-written `pacman.conf` is an
    // msys2 that cannot install anything, and this runs on a machine that
    // may lose power at any point in a twenty-minute install.
    let temporary = conf.with_extension("conf.proxspace-tmp");
    fs::write(&temporary, updated.as_bytes()).map_err(|source| PacmanError::Io {
        action: "write",
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, conf).map_err(|source| PacmanError::Io {
        action: "replace",
        path: conf.to_path_buf(),
        source,
    })?;

    if names.is_empty() {
        ui.detail(&format!("removed the pin block from `{}`", conf.display()));
    } else {
        ui.detail(&format!(
            "pinned {} in `{}`",
            names.join(", "),
            conf.display()
        ));
    }
    Ok(true)
}

/// The names currently pinned by our block.
pub fn ignored(conf: &Path) -> Result<Vec<String>, PacmanError> {
    let text = fs::read_to_string(conf).map_err(|source| PacmanError::Io {
        action: "read",
        path: conf.to_path_buf(),
        source,
    })?;
    Ok(managed_ignores(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::core::pacman::MANAGED_BEGIN;

    const PIN: &str = "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils";

    fn conf_path() -> PathBuf {
        PathBuf::from("etc/pacman.conf")
    }

    #[test]
    fn a_half_deleted_block_is_refused_rather_than_guessed_at() {
        let broken = format!("[options]\n{MANAGED_BEGIN}\nIgnorePkg = {PIN}\n");
        let error = block_error(with_ignores(&broken, &[PIN]).unwrap_err(), &conf_path());
        assert!(matches!(error, PacmanError::BrokenBlock { .. }));
        assert!(error.to_string().contains("edited by hand"));
    }

    #[test]
    fn a_conf_without_an_options_section_is_refused() {
        let error = block_error(
            with_ignores(
                "[ucrt64]\nInclude = /etc/pacman.d/mirrorlist.mingw\n",
                &[PIN],
            )
            .unwrap_err(),
            &conf_path(),
        );
        assert!(matches!(error, PacmanError::NoOptionsSection { .. }));
    }
}
