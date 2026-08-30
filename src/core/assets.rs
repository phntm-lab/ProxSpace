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

use std::path::{Path, PathBuf};

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
/// The build script `autobuild` hands over to.
pub const AUTOBUILD_PATH: &str = "opt/proxspace/autobuild.sh";
/// Templates that script copies into every archive it packs.
pub const AUTOBUILD_ASSET_DIR: &str = "opt/proxspace/autobuild";

const HOOK: &str = include_str!("../../assets/09-proxspace_setup.post");
const NSSWITCH: &str = include_str!("../../assets/nsswitch.conf");
const PM3_WRAPPER: &str = include_str!("../../assets/bin/pm3-wrapper");
const PS_SHIM: &str = include_str!("../../assets/bin/ps-shim");
const AUTOBUILD: &str = include_str!("../../assets/autobuild.sh");

/// The package set, as shipped. Parsed by `packages.rs`; written into the tree
/// unchanged so that what was installed can be read back from the tree itself.
pub const PACKAGES: &str = include_str!("../../assets/packages.txt");

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

/// What goes into a release archive alongside the client: the batch files of
/// the original, one set per fork. They are read by `cmd.exe` on a machine that
/// has no msys2 on it, which is why they are the only assets written with CRLF
/// and the only ones this binary never runs itself.
const AUTOBUILD_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "opt/proxspace/autobuild/official",
        "Go.bat",
        include_str!("../../assets/autobuild/official/Go.bat"),
    ),
    (
        "opt/proxspace/autobuild/official",
        "FLASH - All.bat",
        include_str!("../../assets/autobuild/official/FLASH - All.bat"),
    ),
    (
        "opt/proxspace/autobuild/official",
        "FLASH - Bootrom.bat",
        include_str!("../../assets/autobuild/official/FLASH - Bootrom.bat"),
    ),
    (
        "opt/proxspace/autobuild/official",
        "FLASH - fullimage.bat",
        include_str!("../../assets/autobuild/official/FLASH - fullimage.bat"),
    ),
    (
        "opt/proxspace/autobuild/official/client",
        "setup.bat",
        include_str!("../../assets/autobuild/official/client/setup.bat"),
    ),
    (
        "opt/proxspace/autobuild/rrg",
        "pm3.bat",
        include_str!("../../assets/autobuild/rrg/pm3.bat"),
    ),
    (
        "opt/proxspace/autobuild/rrg",
        "pm3-flash-all.bat",
        include_str!("../../assets/autobuild/rrg/pm3-flash-all.bat"),
    ),
    (
        "opt/proxspace/autobuild/rrg",
        "pm3-flash-bootrom.bat",
        include_str!("../../assets/autobuild/rrg/pm3-flash-bootrom.bat"),
    ),
    (
        "opt/proxspace/autobuild/rrg",
        "pm3-flash-fullimage.bat",
        include_str!("../../assets/autobuild/rrg/pm3-flash-fullimage.bat"),
    ),
    (
        "opt/proxspace/autobuild/rrg/client",
        "setup.bat",
        include_str!("../../assets/autobuild/rrg/client/setup.bat"),
    ),
];

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

    assets.push(Asset {
        path: parent_of(AUTOBUILD_PATH),
        name: name_of(AUTOBUILD_PATH).to_string(),
        contents: normalise(AUTOBUILD),
        executable: true,
    });

    for (path, name, contents) in AUTOBUILD_TEMPLATES {
        assets.push(Asset {
            path,
            name: (*name).to_string(),
            contents: windows_text(contents),
            executable: false,
        });
    }

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

/// Force LF endings.
///
/// The assets are shell scripts read by bash inside msys2, where a trailing
/// `\r` becomes part of the last argument on the line and produces errors that
/// name characters nobody typed. Whether git checked them out with CRLF on this
/// machine must not be able to decide that.
fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Force CRLF endings.
///
/// The mirror image of [`normalise`], for the batch files that leave with a
/// release archive: they are read by `cmd.exe` on a machine with no msys2 on
/// it, and how this repository happened to be checked out must not decide what
/// a user two downloads away gets.
fn windows_text(text: &str) -> String {
    normalise(text).replace('\n', "\r\n")
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

    /// Whether an asset is one of the batch files that go into a release
    /// archive rather than into the environment.
    fn is_template(asset: &Asset) -> bool {
        asset.path.starts_with(AUTOBUILD_ASSET_DIR)
    }

    #[test]
    fn no_asset_read_inside_the_shell_carries_windows_line_endings() {
        for asset in assets().into_iter().filter(|asset| !is_template(asset)) {
            assert!(
                !asset.contents.contains('\r'),
                "{} has CRLF endings",
                asset.name
            );
        }
    }

    /// The other half of the same rule: what leaves in a release archive is
    /// read by `cmd.exe`, and a batch file with bare LF endings is a class of
    /// bug that only ever shows up on the user's machine.
    #[test]
    fn every_archive_template_carries_windows_line_endings() {
        let templates: Vec<Asset> = assets().into_iter().filter(is_template).collect();
        assert_eq!(templates.len(), AUTOBUILD_TEMPLATES.len());

        for template in templates {
            assert!(template.name.ends_with(".bat"), "{}", template.name);
            for line in template.contents.split_inclusive('\n') {
                assert!(
                    line.ends_with("\r\n"),
                    "{} has a bare LF: {line:?}",
                    template.name
                );
            }
            // The frames of the original were already mojibake by the time it
            // shipped them; nothing outside ASCII survives into an archive,
            // where the console codepage is anybody's guess.
            assert!(
                template.contents.is_ascii(),
                "{} is not plain ASCII",
                template.name
            );
        }
    }

    #[test]
    fn the_build_script_reaches_for_the_templates_where_they_are_installed() {
        let script = asset_named("autobuild.sh");

        assert_eq!(
            script.destination(Path::new("root")),
            Path::new("root").join("opt/proxspace/autobuild.sh")
        );
        assert!(
            script
                .contents
                .contains("assetDir=/opt/proxspace/autobuild")
        );
        assert!(script.contents.contains("$assetDir/rrg/*"));
        assert!(script.contents.contains("$assetDir/official/*"));
        // Where the archives go is an argument, with the mount as the fallback
        // — a mount is not always visible to a process that has just started.
        assert!(script.contents.contains(r#"copyDir="${1:-/builds}""#));
        // Recovery images are taken by extension: upstream has renamed them.
        assert!(script.contents.contains("recovery/*.bin"));
        // /setup is not mounted any more, and installing a package is the
        // binary's job: a shell script must not change the machine.
        for forbidden in ["/setup", "pacman"] {
            assert!(
                !script.contents.contains(forbidden),
                "the build script still mentions `{forbidden}`"
            );
        }
    }

    #[test]
    fn the_build_script_builds_for_the_subsystem_this_binary_installs() {
        let script = asset_named("autobuild.sh");

        // The prefix comes from the shell, which computes it from MSYSTEM; the
        // fallback is what that computation gives for the subsystem installed
        // here, and the guard makes sure it is the one the script runs under.
        assert!(
            script
                .contents
                .contains("prefixDir=${MSYSTEM_PREFIX:-/ucrt64}")
        );
        assert!(script.contents.contains(&format!(
            "[ \"$MSYSTEM\" != \"{}\" ]",
            crate::core::msys2::MSYSTEM
        )));
        // The DLLs beside the client and the Qt plugin must come from the same
        // prefix the client was linked against, or the archive fails on the
        // machine it is unpacked on rather than on this one.
        assert!(script.contents.contains("grep \"=> $prefixDir\""));
        assert!(
            script
                .contents
                .contains("$prefixDir/share/qt6/plugins/platforms/qwindows.dll")
        );
    }

    /// Nothing this build ships may still point at the MINGW64 subsystem: a
    /// single leftover path picks up a toolchain that is not installed.
    #[test]
    fn no_asset_mentions_the_subsystem_this_build_left_behind() {
        for asset in assets() {
            for forbidden in ["/mingw64", "MINGW64", "mingw64/"] {
                assert!(
                    !asset.contents.contains(forbidden),
                    "{} still mentions `{forbidden}`",
                    asset.name
                );
            }
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

    /// The tree is the root of its own mount table, so `cygpath -u` on it
    /// gives `/` and the install directory cannot be reached by taking the
    /// parent on the POSIX side — the whole path has to be walked up in
    /// Windows form, trailing backslash first. Getting this wrong points every
    /// shim at `//proxspace.exe`, and only a real tree ever says so.
    #[test]
    fn the_hook_walks_up_to_the_install_directory_on_the_windows_side() {
        let hook = asset_named("09-proxspace_setup.post");

        assert!(hook.contents.contains(r#"ps_tree_win="$(cygpath -w /)""#));
        assert!(hook.contents.contains(r#"ps_tree_win="${ps_tree_win%\\}""#));
        assert!(
            hook.contents
                .contains(r#"ps_base_win="${ps_tree_win%\\*}""#)
        );
        assert!(
            hook.contents
                .contains(r#"PROXSPACE_EXE="$(cygpath -u "$ps_base_win")/proxspace.exe""#)
        );
        assert!(
            !hook.contents.contains(r#"dirname "$(cygpath -u"#),
            "the POSIX round trip is back; it resolves the tree to `/`"
        );
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
            let expected = asset.path == BIN_DIR || asset.name == name_of(AUTOBUILD_PATH);
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
