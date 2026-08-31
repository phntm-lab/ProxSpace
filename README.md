**English** · [Русский](README_RU.md)

# ProxSpace

[![Latest Release](https://img.shields.io/github/v/release/phntm-lab/ProxSpace?style=for-the-badge&logo=github&color=blue)](https://github.com/phntm-lab/ProxSpace/releases/latest)
[![Total Downloads](https://img.shields.io/github/downloads/phntm-lab/ProxSpace/total?style=for-the-badge&logo=github&color=success)](https://github.com/phntm-lab/ProxSpace/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/phntm-lab/ProxSpace/ci.yml?branch=dev&style=for-the-badge&logo=githubactions&logoColor=white&label=CI)](https://github.com/phntm-lab/ProxSpace/actions/workflows/ci.yml)
[![GitHub License](https://img.shields.io/github/license/phntm-lab/ProxSpace?style=for-the-badge&color=orange)](LICENSE)
[![Telegram](https://img.shields.io/endpoint?url=https%3A%2F%2Ftg.sumanjay.workers.dev%2Fphntmlab&style=for-the-badge&logo=telegram&logoColor=white&label=Telegram&color=5865F2)](https://t.me/phntmlab)

A Proxmark3 development environment for Windows, as a single executable.

Put `proxspace.exe` in an empty folder and run it. It downloads msys2, unpacks
it next to itself, configures it, installs the toolchain needed to build and
run [Proxmark3](https://github.com/RfidResearchGroup/proxmark3), and drops you
into a shell. Everything it creates lives in that one folder. It provisions the
environment and nothing else: it does not wrap the proxmark3 client, manage
firmware or talk to the device.

## Releases

Every release is one file, `proxspace.exe`, on the
[releases page](https://github.com/phntm-lab/ProxSpace/releases/latest). There
is nothing else to download and nothing to install.

`proxspace update` asks GitHub whether a newer one has been published and says
so with a link; the binary never replaces itself, and a failed check never gets
in the way of the update it was part of.

## Requirements

|         |                                                                                        |
| ------- | -------------------------------------------------------------------------------------- |
| OS      | 64-bit Windows                                                                         |
| Disk    | ~10 GB free, 15 GB recommended                                                         |
| Network | ~1 GB on the first run                                                                 |
| Path    | only `A-Z a-z 0-9 . _ - \ /` and a drive letter — no spaces, no brackets, no non-ASCII |

The path rule is enforced: nothing downstream quotes that path, so `proxspace`
refuses to start rather than fail later in a way that is hard to diagnose.

## Quick start

1. Create a folder whose path passes the rule above — `C:\ProxSpace` or
   `D:\dev\proxspace` are fine, `C:\My Projects\ProxSpace` is not.
2. Put `proxspace.exe` in it and run it. The first run takes a while; Ctrl+C
   stops it and running it again resumes.
3. In the shell you get, build the firmware:

   ```
   git clone https://github.com/RfidResearchGroup/proxmark3.git
   cd proxmark3
   make clean && make -j
   ```

4. Run the client with `pm3`, flash with `pm3-flash-all`. Both are Proxmark3's
   own scripts; ProxSpace only provides the environment they need.

## Commands

| Command                            | What it does                                         |
| ---------------------------------- | ---------------------------------------------------- |
| `proxspace`                        | Prepare the environment if needed, then open a shell |
| `proxspace shell [-- args]`        | The same, with arguments passed to the login shell   |
| `proxspace install [--force]`      | Install the ProxSpace package set                    |
| `proxspace update [flags]`         | Update the environment                               |
| `proxspace repair [--rebase]`      | Reinstall packages over a broken tree                |
| `proxspace info`                   | Report versions, paths and toolchain state           |
| `proxspace mirrors rank\|restore`  | Reorder or restore pacman mirrors                    |
| `proxspace exec -- <cmd>`          | Run one command inside the environment               |
| `proxspace autobuild`              | Build every Proxmark3 checkout in `pm3/`             |
| `proxspace clean [--cache\|--all]` | Free disk space, or remove the environment           |

Global flags, accepted before or after the subcommand:

| Flag              | Effect                                                     |
| ----------------- | ---------------------------------------------------------- |
| `-y`, `--yes`     | Answer every question affirmatively, for unattended runs   |
| `-q`, `--quiet`   | Print only warnings, errors and command output             |
| `-v`, `--verbose` | Print the detail that normally only goes to the log        |
| `--no-color`      | Never colourise output                                     |
| `--dir <path>`    | Work on a folder other than the one holding the executable |

### shell

Opens a login shell in `pm3/`, which is `$HOME` and `/pm3` inside it. The
environment is brought up first if anything is missing, so there is no separate
install step to remember. `proxspace shell -- -c "make -j"` is the
non-interactive form, and its exit code is passed back.

### install

Installs the package set this build ships. Normally implied by `shell`;
`--force` reinstalls every package on top of itself, which is what a moved
folder or a half-finished install needs.

### update

Both halves run by default.

| Flag                | Effect                                                                                                 |
| ------------------- | ------------------------------------------------------------------------------------------------------ |
| `--msys2`           | The msys2 base system only (`pacman -Syuu`, or a full reinstall of a tree too old to upgrade in place) |
| `--packages`        | The ProxSpace package set only                                                                         |
| `--check`           | Say what each half would do and change nothing                                                         |
| `--reinstall-msys2` | Replace the tree even if it could be upgraded                                                          |
| `--no-reinstall`    | Report instead of replacing, whatever the version                                                      |

### repair

Reinstalls every installed package on top of itself. This is for a tree whose
files are wrong in ways nothing can work out from the outside — a half-written
package, a keyring that was never initialised, damaged databases.

`--rebase` additionally recomputes the load addresses of the msys2 DLLs
(`rebaseall`). It is slow, it needs every ProxSpace process closed, and it fixes
exactly one symptom: builds failing at random with `unable to remap`.

### info

Prints versions, paths, the msys2 and package state, the toolchain it can find
and a little about the machine, and writes the same text to
`proxspace-info.txt` so it can be attached to a bug report. It is the one
command that works on a broken or missing install.

### mirrors

`mirrors rank` measures every pacman mirror and reorders the lists fastest
first, keeping the shipped order in a backup. `mirrors restore` puts the
shipped order back. Worth trying when downloads crawl or time out.

### exec

`proxspace exec -- gcc --version` runs one command in the environment and
returns its exit code. Each word is passed through untouched; use
`exec -- bash -c "..."` when you want shell syntax.

### autobuild

Builds every Proxmark3 checkout in `pm3/` and packs each one into
`builds/<name>/<name>-<date>-<hash>.7z`. For each checkout it pulls
(`git pull --ff-only`), skips it if an archive for that commit already exists,
builds it, and collects the client, the firmware, the recovery images, the
Windows driver and the DLLs the client needs, so that the archive runs on a
machine with no ProxSpace on it.

`p7zip` is installed on demand the first time, and `builds/` is mounted as
`/builds` only while the command runs.

### clean

`--cache` (the default) empties the pacman download cache — gigabytes, at the
cost of a slower reinstall. `--all` removes the msys2 tree entirely. Neither
touches `pm3/` or `builds/`: everything they remove can be downloaded again,
and nothing you made is in them.

## What it creates

Everything lives next to the executable — nothing is written to `%APPDATA%`,
`%TEMP%` or your user profile, so the folder can be moved or copied whole:

```
<folder>/
├── proxspace.exe
├── msys2/                  the msys2 tree
├── pm3/                    your home directory inside the shell; Proxmark3 sources
├── builds/                 output of `proxspace autobuild`
├── proxspace.state.json    what has been installed so far
├── proxspace-info.txt      the last report from `proxspace info`
└── proxspace.log           what happened, including external command output
```

Moving or renaming the folder is supported. The next run notices, says so, and
offers to reinstall the packages — they record the path they were installed
under, so a moved environment needs that before it builds again.

## Inside the shell

`MSYSTEM` is `UCRT64`, `$HOME` is `/pm3`, and `/opt/proxspace/bin` is on
`$PATH`. That directory holds:

| Name                                                                                    | What it is                                                                                                       |
| --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `pm3`, `pm3-flash`, `pm3-flash-all`, `pm3-flash-bootrom`, `pm3-flash-fullimage`         | Wrappers that run the script of the same name from the checkout you are standing in, so `pm3` works without `./` |
| `proxspace`                                                                             | The executable outside, reachable by name inside                                                                 |
| `ps-setup`, `ps-info`, `ps-repair`, `ps-upgrade`, `ps-rankmirrors`, `ps-restoremirrors` | The names the original ProxSpace used, each now a one-line shim for the matching subcommand                      |

## What gets installed

msys2 `20260611`, downloaded from `mirror.msys2.org` and verified against a
sha256 built into the executable, plus the package set in
`assets/packages.txt`: `git`, `make`, `pkgconf`, `base-devel`, `procps-ng`,
the UCRT64 toolchain (`gcc`, `gdb`, `cmake`), the ARM cross toolchain
(`arm-none-eabi-gcc`, `newlib`, `gdb-multiarch`, and a pinned `binutils`),
`qt6-base`, `readline`, `libsndfile`, `lua`, `bzip2`, `jansson`, `openssl`,
`lz4`, `libgd`, `opencl-icd`, `openocd` and Python with `pip`, `setuptools` and
`cryptography`. The ChameleonMini/AVR packages of the original are shipped
commented out.

The list is written into the tree as `/opt/proxspace/packages.txt`, so what was
installed can be read back from the environment itself.

## Exit codes

| Code  | Meaning                                                  |
| ----- | -------------------------------------------------------- |
| `0`   | It did what it was asked                                 |
| `1`   | It failed, and said why on stderr and in `proxspace.log` |
| `2`   | The command line was wrong                               |
| `130` | Stopped by Ctrl+C                                        |

`shell`, `exec` and `autobuild` hand back the exit code of the program they
ran instead.

## Troubleshooting

| Symptom                                                              | What to do                                                                                                                                                                                                         |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| It refuses to start and names a character in the path                | Move the folder somewhere without spaces, brackets or non-ASCII characters                                                                                                                                         |
| Downloads crawl, time out, or pacman cannot reach the mirrors        | `proxspace mirrors rank`, then the command that failed; `mirrors restore` puts the shipped order back                                                                                                              |
| A package signature cannot be verified, or the databases are damaged | `proxspace repair` — it reinstalls everything over itself and refreshes the keyring and databases                                                                                                                  |
| Builds fail at random with `unable to remap` or a `fork` error       | Close every ProxSpace window, then `proxspace repair --rebase`. A cygwin DLL base-address collision; the original ran `rebaseall` on every start to avoid it                                                       |
| pacman says the database is locked                                   | Another pacman is running — close any other ProxSpace window. A lock left by a killed pacman is cleared automatically                                                                                              |
| "The file is in use" on remove or replace                            | The message names the process holding it: an open shell, an editor, a build, `gpg-agent`, or an antivirus scanning the tree. Close it and run the command again                                                    |
| An install or update was interrupted                                 | Run the same command again. Every completed step is in `proxspace.state.json`, so it carries on instead of starting over                                                                                           |
| `update --check` takes half a minute before printing                 | It asks GitHub whether a newer ProxSpace was released. With no network that wait is the TLS handshake timing out; the rest of the command works normally                                                           |
| Anything else                                                        | `proxspace info` writes what the environment turned out to be to `proxspace-info.txt`; `proxspace.log` holds every message and the full output of every external command. Both are worth attaching to a bug report |

`proxspace.log` is kept across runs; once it passes 5 MB it is moved aside as
`proxspace.log.old` and started again.

## Differences from the original ProxSpace

This is a rewrite of [Gator96100/ProxSpace](https://github.com/Gator96100/ProxSpace),
which shipped msys2 inside the repository and drove it with `.bat` and shell
scripts. The behaviour differs deliberately:

|                                                |                                                                                                                                                                                                       |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One executable                                 | Instead of `runme64.bat`, `autobuild.bat`, `setup.cmd` and `setup/bin/*`; the old names still exist as shims inside the shell                                                                         |
| msys2 is downloaded, not vendored              | The repository stays small and the base system is verified by checksum on the way in                                                                                                                  |
| UCRT64 instead of MINGW64                      | `MSYSTEM=UCRT64`, `PYTHONHOME=/ucrt64`, and the `mingw-w64-ucrt-x86_64-*` packages                                                                                                                    |
| The install is an explicit resumable pipeline  | The original relied on launching the shell twice, because `pacman -Syuu` cannot continue in a process whose msys2 runtime it just replaced. Each completed step is recorded in `proxspace.state.json` |
| Nothing is installed from inside a login shell | The original's `09-proxspace_setup.post` ran pacman as part of logging in; here the hook only sets up the environment                                                                                 |
| There is a log                                 | Everything printed, plus the full output of every external command, is mirrored to `proxspace.log`                                                                                                    |
| `rebaseall` does not run on every start        | It is available as `proxspace repair --rebase`                                                                                                                                                        |
| There is a way back out                        | `proxspace clean` frees the cache or removes the environment without touching `pm3/` and `builds/`                                                                                                    |

## Development

Everything below is for someone changing ProxSpace itself. How to send a
change — the checks, the commit style, what a pull request is expected to
carry — is in [CONTRIBUTING.md](.github/CONTRIBUTING.md).

### Building and checking

| Command                                     | What it is for                                                                 |
| ------------------------------------------- | ------------------------------------------------------------------------------ |
| `cargo build --release`                     | The shipped binary: thin LTO, one codegen unit, symbols stripped               |
| `cargo test`                                | Everything, in seconds — nothing here touches the network or a real msys2 tree |
| `cargo test -- --ignored`                   | The three tests that do: the real base archive and a real mirror               |
| `cargo fmt --check`                         | Formatting, exactly as CI runs it                                              |
| `cargo clippy --all-targets -- -D warnings` | Lints; warnings are errors here and in CI                                      |

The toolchain floor is `rust-version` in `Cargo.toml` (1.85, edition 2024).

`--dir <path>` makes every command work on a folder other than the one holding
the executable, which is how a debug build is pointed at a scratch tree instead
of at the one next to `target/debug/`.

### Module map

The logic lives in `src/lib.rs` rather than in `main.rs` so the integration
tests can drive it directly instead of only through the command line.

The modules sit in layers, and a layer may only name itself or a layer further
in:

| Layer   | Modules                                                                                                                 | What it holds                                                                                                                                                                                                                          |
| ------- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `core`  | `paths`, `state`, `packages`, `plan`, `update`, `versions`, `preflight`, `pacman`, `assets`, `fstab`, `userdb`, `msys2` | Data and decisions: where things go, what the state file means, which packages and which msys2 this build installs, what an update does to a tree, what `pacman.conf` should say. Reachable with no disk, no network and no subprocess |
| `ui`    | `ui`, `logging`, `interrupt`                                                                                            | Every message the user sees, the log it is mirrored to, Ctrl+C                                                                                                                                                                         |
| `ports` | `command`, `http`                                                                                                       | The two ways out of the process that have a second implementation in tests: running another program, and the network                                                                                                                   |
| `infra` | `process`, `http`, `download`, `archive`, `assets`, `paths`, `preflight`, `state`, `pacman`, `msys2`                    | Everything that actually touches the disk, the network or another program: spawning it, fetching and unpacking, writing the assets, the account files and `pacman.conf`, reading the state file back                                   |
| `app`   | `install`, `repair`, `provision`, `update`, `clean`, `autobuild`, `info`, `mirrors`, `release`                          | What each command does, stage by stage                                                                                                                                                                                                 |
| `cli`   | `args`, `dispatch`                                                                                                      | The command tree clap parses, and the one `match` every command goes through                                                                                                                                                           |

Several names appear twice — `assets`, `paths`, `preflight`, `state`, `pacman`,
`msys2`, `http`. That is the split, not a duplicate: in `core` the module says
what the answer is, in `infra` it reads or writes the file that carries it.
`core::pacman` decides what `pacman.conf` should look like with our pin in it;
`infra::pacman::conf` writes that file.

`core` and `ui` are peers at the innermost rank and neither may name the other:
the decisions must not know how they are shown, and the screen must not know
what is being decided. `tests/layers.rs` reads `src/` and enforces all of this
— in a `use`, in an inline path and in a doc link alike.

Two more rules hold the shape together: nothing runs an external program except
through `ports::command::CommandRunner`, and nothing reaches the network except
through `ports::http::HttpClient`. Both are traits with fakes in tests, which is
why the suite runs offline. Everything else that leaves the process has no
second implementation and no trait.

The install pipeline is a ladder of stages recorded in the state file —
`NotInstalled → Downloaded → Extracted → Bootstrapped → CoreUpdated →
PackagesInstalled → Ready`. Each step is idempotent and resumes from the last
completed one; a new step means a new `Stage` variant and its place in that
order.

### Tests

`src/*` carries the unit tests; `tests/` holds the integration ones, each file
covering one seam: `install_flow` (the pipeline end to end against fakes),
`cli`, `state`, `archive`, `update_matrix` (every installed-version × build
combination and what it decides), `download`, `provision`, `prepare`,
`resources`, `docs` (the command tables of both READMEs against the clap tree),
`release_tag`, and `layers` (the dependency rule above). Fixtures — a real
`pacman.conf`, `fstab` samples — are in `tests/fixtures/`.

Tests are named as sentences about behaviour, not after the function under
test, and each one states a decision the code makes. Please keep it that way.

### Windows resources, `build.rs` and `rc.exe`

`build.rs` embeds an icon, a version resource and a manifest with `winresource`.
`FileVersion` and `ProductVersion` come from `CARGO_PKG_VERSION`; the rest is
set in the script. It only runs on Windows targets.

Doing that needs a resource compiler — `rc.exe` from the Windows SDK, or
`windres` from a mingw toolchain. A machine that has neither still builds a
working `proxspace.exe`: the failure is a `cargo:warning` and the binary comes
out plain, because failing the build over the icon would be the wrong trade.

The cost is that a broken icon or a mistyped manifest also shows up only as that
warning. `tests/resources.rs` reads both files the way the resource compiler
will — the icon directory, the sizes it declares, the manifest as XML — so a
mistake in them fails the suite instead of shipping quietly.

### Assets and how they reach the msys2 tree

The original kept these next to the executable in `setup/` and mounted it as
`/setup`. Here they live in `assets/`, are compiled into the binary with
`include_str!`, and are written into the tree by `infra::assets::install()`.
Nothing but `msys2/`, `pm3/` and the bookkeeping files ever appears next to the
binary, and the assets cannot be a version out of step with the code that uses
them.

| Source                               | Lands at                                   | What it is                                                                                                                  |
| ------------------------------------ | ------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `assets/09-proxspace_setup.post`     | `etc/post-install/09-proxspace_setup.post` | Sourced at the end of every login: `$PATH`, `MSYSTEM`, `PSVERSION`                                                          |
| `assets/nsswitch.conf`               | `etc/nsswitch.conf`                        | Makes cygwin read users from `/etc/passwd` only, never from Windows                                                         |
| `assets/bin/pm3-wrapper`             | `opt/proxspace/bin/pm3*`                   | One script under five names; dispatches on `basename $0`                                                                    |
| `assets/bin/ps-shim`                 | `opt/proxspace/bin/ps-*`, `proxspace`      | One template per subcommand of the original's `setup/bin`                                                                   |
| `assets/packages.txt`                | `opt/proxspace/packages.txt`               | The package set, verbatim, so it can be read back from the tree                                                             |
| `assets/autobuild.sh`                | `opt/proxspace/autobuild.sh`               | The build script `autobuild` hands over to                                                                                  |
| `assets/autobuild/{official,rrg}/**` | `opt/proxspace/autobuild/**`               | `.bat` files copied into every release archive; the only assets written with CRLF, and the only ones this binary never runs |

Three are templates: `@PSVERSION@` in the hook and `@COMMAND@` in the shim are
substituted from compile-time constants and a fixed table in `assets.rs`.
Nothing substituted depends on where the folder is installed — that is what lets
an installed tree be copied to another machine.

Adding an asset means adding the file, the `include_str!`, and its row in
`assets::assets()`; `install()` writes anything listed there, normalises line
endings and marks scripts executable.

### Bumping msys2

Every version-specific constant is in one block at the top of
`src/core/msys2.rs`, and bumping msys2 means editing that block as a unit:

1. Pick the newest `msys2-base-x86_64-<datestamp>.tar.xz` from
   <https://repo.msys2.org/distrib/x86_64/> and put its datestamp in
   `MSYS2_VERSION`.
2. Point `MSYS2_URL` at that file — the archive's file name is parsed back out
   of the URL, so it has to keep the upstream name.
3. Download it, hash it, cross-check the hash against a second mirror, and put
   it in `MSYS2_SHA256`. Upstream publishes no checksum file, so this constant
   is the only thing between a substituted download and an install.
4. Leave `MSYS2_MIN_COMPATIBLE` alone unless upstream broke the upgrade path
   from older runtimes. Raising it means everyone below that version gets a full
   reinstall instead of `pacman -Syuu`.

Versions are compared as strings, which works only while the datestamp form
holds; the tests in that module check the constants against each other and
against that form, and `tests/update_matrix.rs` covers what each
installed-version × build combination decides.

### Changing the package set

`assets/packages.txt` is the source of truth: one package per line, `#` and `;`
comments, and — for the pinned `arm-none-eabi-binutils` — a full
`.pkg.tar.zst` URL. The URL form exists because the repository version of that
package breaks the proxmark3 build; the name and version are dug out of the file
name in the URL, so it has to keep pacman's `name-version-release-arch` shape.
`packages.rs` validates all of this and rejects a typo at parse time rather than
in the middle of an install.

Moving the pin to a new version is a matter of editing that one line; `pacman.rs`
keeps the `IgnorePkg` block in `pacman.conf` in step with whatever is pinned, so
an upgrade does not quietly replace it.

Changing the list does not need any other edit — it is shipped into the tree and
parsed from the same string.

### Conventions

The reasoning lives in the module headers. Every `src/*.rs` opens with a `//!`
block saying what the module is for, what the original ProxSpace did in its
place, and why anything that looks arbitrary is the way it is — the pin, the
character class in `preflight`, the stage ladder, `-Scc` over `-Sc`. Before
changing something that looks pointless, read the header above it; before adding
something, leave the same kind of note behind.

Comments are in English, whatever the language of the discussion that produced
them, and explain the technical reason rather than the history. `cargo doc
--no-deps --document-private-items` renders all of it if that is easier to read
than the source.

Tests are named as sentences about behaviour, and a behaviour change without a
test that fails before it is not finished. New dependencies want a reason in the
pull request: the tree is deliberately small and every crate in it is either
doing something that cannot be written in an afternoon or is already a
transitive dependency.

## Licence

MIT, in [LICENSE](LICENSE). This is a rewrite rather than a fork: none of the
original ProxSpace is in it, and the batch files a build archive carries are
ours, written to do what the originals did.

Contributions are taken under the same licence; how to send one is in
[CONTRIBUTING.md](.github/CONTRIBUTING.md).
