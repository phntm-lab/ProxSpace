//! Files ProxSpace puts inside the msys2 tree, carried in the binary itself.
//!
//! The original kept these next to the executable in `setup/` and mounted that
//! directory as `/setup`. Here they are compiled in with [`include_str!`] and
//! written into the tree at `opt/proxspace/`, which means
//! nothing but `msys2/`, `pm3/` and two bookkeeping files ever appears next to
//! the binary, and the assets can never be a version out of step with the code
//! that relies on them.
//!
//! Three of them are templates rather than literals — the login hook needs this
//! build's version number, and the `ps-*` shims differ only in which subcommand
//! they hand back to `proxspace.exe`. Everything substituted is either a
//! compile-time constant or a fixed string from the table below; nothing here
//! depends on where the folder happens to be installed, which is what lets the
//! whole thing be copied to another machine unchanged.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::ui::Ui;

/// Where the assets live inside the tree, relative to the msys2 root.
pub const PROXSPACE_DIR: &str = "opt/proxspace";
/// Directory put on `$PATH` by the login hook.
pub const BIN_DIR: &str = "opt/proxspace/bin";
/// The package list, materialised so that `info` and the user can read it.
pub const PACKAGES_PATH: &str = "opt/proxspace/packages.txt";
/// Sourced by `/etc/profile` at the end of every login.
pub const HOOK_PATH: &str = "etc/post-install/09-proxspace_setup.post";
/// Makes cygwin read users from `/etc/passwd` only, never from Windows.
pub const NSSWITCH_PATH: &str = "etc/nsswitch.conf";

const HOOK: &str = include_str!("../assets/09-proxspace_setup.post");
const NSSWITCH: &str = include_str!("../assets/nsswitch.conf");
const PM3_WRAPPER: &str = include_str!("../assets/bin/pm3-wrapper");
const PS_SHIM: &str = include_str!("../assets/bin/ps-shim");

/// The package set, as shipped. Parsed by `packages.rs`; written into the tree
/// unchanged so that what was installed can be read back from the tree itself.
pub const PACKAGES: &str = include_str!("../assets/packages.txt");

/// Placeholder in the login hook, replaced with this build's version.
const VERSION_PLACEHOLDER: &str = "@PSVERSION@";
/// Placeholder in the shim template, replaced with the command line below.
const COMMAND_PLACEHOLDER: &str = "@COMMAND@";

/// Names the proxmark3 client's own scripts are known by. One wrapper serves
/// all of them: it dispatches on `basename $0`, exactly as the original did.
const PM3_WRAPPER_NAMES: &[&str] = &[
    "pm3",
    "pm3-flash",
    "pm3-flash-all",
    "pm3-flash-bootrom",
    "pm3-flash-fullimage",
];

/// The `setup/bin` names of the original, mapped to the subcommand each one now
/// stands for. An empty command means "pass everything through", which is what
/// makes `proxspace` inside the shell behave like `proxspace.exe` outside it.
const SHIMS: &[(&str, &str)] = &[
    ("proxspace", ""),
    ("ps-setup", "install"),
    ("ps-info", "info"),
    ("ps-repair", "repair"),
    ("ps-rankmirrors", "mirrors rank"),
    ("ps-restoremirrors", "mirrors restore"),
    ("ps-upgrade", "update"),
];

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("cannot create `{path}`")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// One file to be placed in the msys2 tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Location inside the tree, with forward slashes, always relative.
    pub path: &'static str,
    /// Name it is written under when `path` is a directory of many assets.
    pub name: String,
    /// Final contents, with every placeholder already filled in.
    pub contents: String,
    /// Whether the file is meant to be run rather than read.
    pub executable: bool,
}

impl Asset {
    /// Where this asset goes, given the root of the msys2 tree.
    pub fn destination(&self, root: &Path) -> PathBuf {
        let mut destination = root.to_path_buf();
        for segment in self.path.split('/').filter(|segment| !segment.is_empty()) {
            destination.push(segment);
        }
        if !self.name.is_empty() {
            destination.push(&self.name);
        }
        destination
    }
}

/// Every file this build installs into the tree.
pub fn assets() -> Vec<Asset> {
    let mut assets = vec![
        Asset {
            path: parent_of(HOOK_PATH),
            name: name_of(HOOK_PATH).to_string(),
            contents: normalise(&HOOK.replace(VERSION_PLACEHOLDER, crate::VERSION)),
            // Sourced by /etc/profile, never executed.
            executable: false,
        },
        Asset {
            path: parent_of(NSSWITCH_PATH),
            name: name_of(NSSWITCH_PATH).to_string(),
            contents: normalise(NSSWITCH),
            executable: false,
        },
        Asset {
            path: parent_of(PACKAGES_PATH),
            name: name_of(PACKAGES_PATH).to_string(),
            contents: normalise(PACKAGES),
            executable: false,
        },
    ];

    for name in PM3_WRAPPER_NAMES {
        assets.push(Asset {
            path: BIN_DIR,
            name: (*name).to_string(),
            contents: normalise(PM3_WRAPPER),
            executable: true,
        });
    }

    for (name, command) in SHIMS {
        assets.push(Asset {
            path: BIN_DIR,
            name: (*name).to_string(),
            contents: normalise(&PS_SHIM.replace(COMMAND_PLACEHOLDER, &shim_command(command))),
            executable: true,
        });
    }

    assets
}

/// The argument list a shim passes to `proxspace.exe`. The user's own arguments
/// always come last, so `ps-setup --force` and `proxspace info` both work.
fn shim_command(command: &str) -> String {
    if command.is_empty() {
        "\"$@\"".to_string()
    } else {
        format!("{command} \"$@\"")
    }
}

/// Put every asset in place under `root`, which must be the msys2 tree.
///
/// Idempotent by comparison, not by overwriting: a file that already holds the
/// right bytes is left alone, so the mtimes in the tree keep meaning something
/// and a run that changes nothing says so. A file that was edited by hand is
/// silently restored — these are ours, and the way to change them is to change
/// the binary.
pub fn install(root: &Path, ui: &Ui) -> Result<Report, AssetError> {
    let mut report = Report::default();

    for asset in assets() {
        let destination = asset.destination(root);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| AssetError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        if is_already_there(&destination, &asset.contents) {
            report.unchanged += 1;
        } else {
            fs::write(&destination, asset.contents.as_bytes()).map_err(|source| {
                AssetError::Write {
                    path: destination.clone(),
                    source,
                }
            })?;
            ui.detail(&format!("wrote `{}`", destination.display()));
            report.written.push(destination.clone());
        }

        if asset.executable {
            make_executable(&destination).map_err(|source| AssetError::Write {
                path: destination,
                source,
            })?;
        }
    }

    Ok(report)
}

/// What [`install`] did, for the caller to log or ignore.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub written: Vec<PathBuf>,
    pub unchanged: usize,
}

impl Report {
    pub fn changed_anything(&self) -> bool {
        !self.written.is_empty()
    }
}

fn is_already_there(path: &Path, contents: &str) -> bool {
    // An unreadable file counts as different: the write that follows is the
    // best chance of getting a usable one, and it will report the real error.
    fs::read(path).is_ok_and(|existing| existing == contents.as_bytes())
}

/// Mark a script as runnable.
///
/// On Windows this is a no-op, and deliberately so: the tree is mounted
/// `noacl` (`fstab.rs`), under which cygwin does not consult NTFS permissions
/// at all and calls a file executable when it starts with `#!` — which every
/// script here does. Faking a POSIX mode through the Windows API would change
/// nothing about what the shell sees.
#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Force LF endings.
///
/// The assets are shell scripts read by bash inside msys2, where a trailing
/// `\r` becomes part of the last argument on the line and produces errors that
/// name characters nobody typed. Whether git checked them out with CRLF on this
/// machine must not be able to decide that.
fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Everything before the last `/` of an asset path.
fn parent_of(path: &'static str) -> &'static str {
    match path.rfind('/') {
        Some(index) => &path[..index],
        None => "",
    }
}

/// Everything after the last `/` of an asset path.
fn name_of(path: &'static str) -> &'static str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn silent_ui() -> Ui {
        Ui::new(
            crate::ui::UiOptions {
                quiet: true,
                ..crate::ui::UiOptions::default()
            },
            Arc::new(crate::logging::Logger::disabled()),
        )
    }

    fn asset_named(name: &str) -> Asset {
        assets()
            .into_iter()
            .find(|asset| asset.name == name)
            .unwrap_or_else(|| panic!("no asset called `{name}`"))
    }

    #[test]
    fn every_asset_lands_inside_the_tree() {
        let root = Path::new(r"C:\ProxSpace\msys2");
        for asset in assets() {
            let destination = asset.destination(root);
            assert!(
                destination.starts_with(root),
                "{} escapes the tree",
                destination.display()
            );
            assert!(!asset.name.is_empty(), "{} has no file name", asset.path);
        }
    }

    #[test]
    fn asset_names_are_unique() {
        let mut destinations: Vec<PathBuf> = assets()
            .iter()
            .map(|asset| asset.destination(Path::new("root")))
            .collect();
        let count = destinations.len();
        destinations.sort();
        destinations.dedup();
        assert_eq!(destinations.len(), count, "two assets share a destination");
    }

    #[test]
    fn no_asset_carries_windows_line_endings() {
        for asset in assets() {
            assert!(
                !asset.contents.contains('\r'),
                "{} has CRLF endings",
                asset.name
            );
        }
    }

    #[test]
    fn no_placeholder_survives_into_a_finished_asset() {
        for asset in assets() {
            assert!(
                !asset.contents.contains(VERSION_PLACEHOLDER)
                    && !asset.contents.contains(COMMAND_PLACEHOLDER),
                "{} still has a placeholder:\n{}",
                asset.name,
                asset.contents
            );
        }
    }

    #[test]
    fn the_login_hook_sets_up_ucrt64_and_nothing_else() {
        let hook = asset_named("09-proxspace_setup.post");

        assert!(
            hook.contents
                .contains(&format!("PSVERSION=\"{}\"", crate::VERSION))
        );
        assert!(hook.contents.contains("PYTHONHOME=/ucrt64"));
        assert!(hook.contents.contains("MSYSTEM\" = \"UCRT64"));
        assert!(hook.contents.contains("PATH=/opt/proxspace/bin:$PATH"));
        assert!(hook.contents.contains("PROXSPACE_EXE"));

        // The install logic of the original lived here and now lives in Rust;
        // if any of it comes back, a shell login starts changing the machine.
        for forbidden in ["pacman", "ps-setup", "/setup", "/mingw64", "MINGW64"] {
            assert!(
                !hook.contents.contains(forbidden),
                "the login hook still mentions `{forbidden}`"
            );
        }
    }

    #[test]
    fn shims_hand_the_right_command_back_to_the_binary() {
        assert!(
            asset_named("ps-setup")
                .contents
                .contains("exec \"$PROXSPACE_EXE\" install \"$@\"")
        );
        assert!(
            asset_named("ps-rankmirrors")
                .contents
                .contains("exec \"$PROXSPACE_EXE\" mirrors rank \"$@\"")
        );
        // The passthrough shim must not gain an empty argument, which would
        // reach clap as a subcommand named "" and fail.
        assert!(
            asset_named("proxspace")
                .contents
                .contains("exec \"$PROXSPACE_EXE\" \"$@\"")
        );
    }

    #[test]
    fn every_original_setup_bin_name_still_exists() {
        let names: Vec<String> = assets().into_iter().map(|asset| asset.name).collect();
        for original in [
            "pm3",
            "pm3-flash",
            "pm3-flash-all",
            "pm3-flash-bootrom",
            "pm3-flash-fullimage",
            "ps-info",
            "ps-rankmirrors",
            "ps-repair",
            "ps-restoremirrors",
            "ps-setup",
            "ps-upgrade",
        ] {
            assert!(
                names.iter().any(|name| name == original),
                "`{original}` from the original setup/bin is missing"
            );
        }
    }

    #[test]
    fn scripts_are_executable_and_config_files_are_not() {
        for asset in assets() {
            let expected = asset.path == BIN_DIR;
            assert_eq!(
                asset.executable, expected,
                "{} has the wrong executable flag",
                asset.name
            );
            if asset.executable {
                assert!(
                    asset.contents.starts_with("#!/usr/bin/env bash"),
                    // Without the shebang the noacl mount would not consider
                    // the file executable at all.
                    "{} is executable but has no shebang",
                    asset.name
                );
            }
        }
    }

    #[test]
    fn the_package_list_survives_verbatim() {
        let packages = asset_named("packages.txt");
        assert!(packages.contents.contains("mingw-w64-ucrt-x86_64-gcc\n"));
        assert!(packages.contents.contains(
            "https://repo.msys2.org/mingw/ucrt64/\
             mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.46.1-1-any.pkg.tar.zst"
        ));
        // The ChameleonMini section ships commented out.
        assert!(packages.contents.contains("#mingw-w64-ucrt-x86_64-avrdude"));
        // Nothing from the MINGW64 era may have been left behind.
        assert!(!packages.contents.contains("mingw-w64-x86_64-"));
    }

    #[test]
    fn installing_puts_every_asset_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let report = install(root, &silent_ui()).unwrap();

        assert_eq!(report.written.len(), assets().len());
        assert_eq!(report.unchanged, 0);
        for asset in assets() {
            let destination = asset.destination(root);
            assert_eq!(
                fs::read_to_string(&destination).unwrap(),
                asset.contents,
                "{} was written wrong",
                destination.display()
            );
        }
        assert!(root.join(HOOK_PATH).is_file());
        assert!(root.join(NSSWITCH_PATH).is_file());
        assert!(root.join(PACKAGES_PATH).is_file());
        assert!(root.join(BIN_DIR).join("pm3").is_file());
    }

    #[test]
    fn a_second_install_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &silent_ui()).unwrap();

        let second = install(dir.path(), &silent_ui()).unwrap();

        assert!(!second.changed_anything());
        assert_eq!(second.unchanged, assets().len());
    }

    #[test]
    fn an_edited_asset_is_put_back() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &silent_ui()).unwrap();
        let hook = dir.path().join(HOOK_PATH);
        fs::write(&hook, b"rm -rf /\n").unwrap();

        let report = install(dir.path(), &silent_ui()).unwrap();

        assert_eq!(report.written, vec![hook.clone()]);
        assert_eq!(
            fs::read_to_string(&hook).unwrap(),
            asset_named("09-proxspace_setup.post").contents
        );
    }

    #[test]
    fn paths_are_split_the_way_the_constants_expect() {
        assert_eq!(parent_of(HOOK_PATH), "etc/post-install");
        assert_eq!(name_of(HOOK_PATH), "09-proxspace_setup.post");
        assert_eq!(parent_of("nsswitch.conf"), "");
        assert_eq!(name_of("nsswitch.conf"), "nsswitch.conf");
    }

    #[test]
    fn line_endings_are_normalised_not_doubled() {
        assert_eq!(normalise("a\r\nb\n"), "a\nb\n");
        assert_eq!(normalise("a\nb\n"), "a\nb\n");
    }
}
