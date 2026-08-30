# ProxSpace

A Proxmark3 development environment for Windows, as a single executable.

Put `proxspace.exe` in an empty folder and run it. It downloads msys2, unpacks
it next to itself, configures it, installs the toolchain needed to build and
run [Proxmark3](https://github.com/RfidResearchGroup/proxmark3), and drops you
into a shell. Everything it creates lives in that one folder.

## Quick start

1. Create a folder whose path has no spaces and no non-ASCII characters —
   `C:\ProxSpace` or `D:\dev\proxspace` are fine, `C:\My Projects\ProxSpace`
   is not.
2. Put `proxspace.exe` in it and run it. The first run downloads about a
   gigabyte and takes a while; it can be interrupted with Ctrl+C and resumed by
   running it again.
3. In the shell you get, clone the firmware you want to build:

   ```
   git clone https://github.com/RfidResearchGroup/proxmark3.git
   cd proxmark3
   make clean && make -j
   ```

4. Run the client with `./pm3`, flash with `./pm3-flash-all`. Both are
   Proxmark3's own scripts; ProxSpace only provides the environment they need.

## Status

Every command is implemented and covered by tests, and the whole of it has been
run against a real msys2 tree on Windows 11: install from scratch, update,
repair, mirror ranking, moving the folder, `clean`, a Proxmark3 build and
`autobuild`. What that run found is fixed and written down in
`.claude/ACCEPTANCE.md`. One thing is still only checked by hand: how the
Ctrl+C paths and the confirmation prompts behave in a real console.

## What it is not

This is an environment provisioner, not a Proxmark3 tool. It does not wrap the
proxmark3 client, manage firmware or talk to the device.

## Layout

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

## Commands

| Command | What it does |
|---|---|
| `proxspace` | Prepare the environment if needed, then open a shell |
| `proxspace shell [-- args]` | The same, with arguments passed to the login shell |
| `proxspace install [--force]` | Install the ProxSpace package set |
| `proxspace update [flags]` | Update the environment |
| `proxspace repair [--rebase]` | Reinstall packages over a broken tree |
| `proxspace info` | Report versions, paths and toolchain state |
| `proxspace mirrors rank\|restore` | Reorder or restore pacman mirrors |
| `proxspace exec -- <cmd>` | Run one command inside the environment |
| `proxspace autobuild` | Build every Proxmark3 checkout in `pm3/` |
| `proxspace clean [--cache\|--all]` | Free disk space, or remove the environment |

Global flags: `--yes` (answer every question affirmatively, for unattended
runs), `--quiet`, `--verbose`, `--no-color`, `--dir <path>` (work on a folder
other than the one holding the executable).

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

Two halves, both done by default:

- `--msys2` — the msys2 base system only (`pacman -Syuu`, or a full reinstall
  of the tree when the installed one is too old to be upgraded in place);
- `--packages` — the ProxSpace package set only.

`--check` says what each would do and changes nothing. `--reinstall-msys2`
replaces the tree even if it could be upgraded; `--no-reinstall` reports
instead of replacing, whatever the version.

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
Windows driver and the DLLs the client needs so that the archive runs on a
machine with no ProxSpace on it.

`p7zip` is installed on demand the first time, and `builds/` is mounted as
`/builds` only while the command runs. The build itself is a shell script
inside the environment (`/opt/proxspace/autobuild.sh`); this command mounts,
prepares and runs it.

### clean

`--cache` (the default) empties the pacman download cache — gigabytes, at the
cost of a slower reinstall. `--all` removes the msys2 tree entirely. Neither
touches `pm3/` or `builds/`: everything they remove can be downloaded again,
and nothing you made is in them.

## Inside the shell

`MSYSTEM` is `UCRT64`, `$HOME` is `/pm3`, and `/opt/proxspace/bin` is on
`$PATH`. That directory holds:

- `pm3`, `pm3-flash`, `pm3-flash-all`, `pm3-flash-bootrom`,
  `pm3-flash-fullimage` — wrappers that run the script of the same name from
  the checkout you are standing in, so `pm3` works without `./`;
- `proxspace` — the executable outside, reachable by name inside;
- `ps-setup`, `ps-info`, `ps-repair`, `ps-upgrade`, `ps-rankmirrors`,
  `ps-restoremirrors` — the names the original ProxSpace used, each now a
  one-line shim for the matching subcommand.

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

## Requirements

- 64-bit Windows
- About 10 GB of free disk space (15 GB recommended)
- An install path made only of `A-Z a-z 0-9 . _ - \ /` and a drive letter — no
  spaces, no brackets, no non-ASCII characters. The msys2 toolchain and the
  Proxmark3 makefiles do not survive them, and `proxspace` refuses to start
  rather than fail later in a way that is hard to diagnose.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | It did what it was asked |
| `1` | It failed, and said why on stderr and in `proxspace.log` |
| `2` | The command line was wrong |
| `130` | Stopped by Ctrl+C |

`shell`, `exec` and `autobuild` hand back the exit code of the program they
ran instead.

## Troubleshooting

**It refuses to start and names a character in the path.** Move the folder
somewhere without spaces, brackets or non-ASCII characters. Nothing downstream
quotes that path, so this is refused rather than worked around.

**Downloads crawl, time out, or pacman says it cannot reach the mirrors.** Run
`proxspace mirrors rank`, then the command that failed. `proxspace mirrors
restore` puts the shipped order back.

**A package signature cannot be verified, or the databases are damaged.**
`proxspace repair`. It reinstalls everything over itself and refreshes the
keyring and databases.

**Builds fail at random with `unable to remap` or a `fork` error.** Close every
ProxSpace window and run `proxspace repair --rebase`. This is a cygwin DLL
base-address collision; the original ran `rebaseall` on every single start to
avoid it, which cost minutes a day to fix a problem most installs never have.

**pacman says the database is locked.** Another pacman is running — close any
other ProxSpace window. A lock left behind by a killed pacman is cleared
automatically.

**Something cannot be removed or replaced: "the file is in use".** The message
names the process holding it. It is usually an open shell, an editor, a build,
`gpg-agent`, or an antivirus scanning the tree as it is written. Close it and
run the command again.

**An install or update was interrupted.** Run the same command again. Every
completed step is recorded in `proxspace.state.json`, so it carries on from
where it stopped rather than starting over.

**`proxspace update --check` takes half a minute before printing anything.** It
asks GitHub whether a newer ProxSpace was released. With no network, that wait
is the TLS handshake timing out; the check then reports nothing and the rest of
the command works normally.

**Anything else.** `proxspace info` prints what the environment turned out to
be and writes it to `proxspace-info.txt`; `proxspace.log` holds every message
and the full output of every external command, run after run. Once it passes
5 MB it is moved aside as `proxspace.log.old` and started again. Both files are
worth attaching to a bug report.

## Differences from the original ProxSpace

This is a rewrite of [Gator96100/ProxSpace](https://github.com/Gator96100/ProxSpace),
which shipped msys2 inside the repository and drove it with `.bat` and shell
scripts. The behaviour differs deliberately in a few places:

- **One executable instead of `runme64.bat`, `autobuild.bat`, `setup.cmd` and
  `setup/bin/*`.** The old names still exist as shims inside the shell.
- **msys2 is downloaded, not vendored.** The repository stays small and the
  base system is verified by checksum on the way in.
- **UCRT64 instead of MINGW64.** `MSYSTEM=UCRT64`, `PYTHONHOME=/ucrt64`, and
  the `mingw-w64-ucrt-x86_64-*` packages.
- **The install is an explicit resumable pipeline.** The original relied on
  launching the shell twice, because `pacman -Syuu` cannot continue in a
  process whose msys2 runtime it just replaced. Each completed step is now
  recorded in `proxspace.state.json`, so an interrupted install resumes instead
  of restarting.
- **Nothing is installed from inside a login shell.** The original's
  `09-proxspace_setup.post` ran pacman as part of logging in; here the hook only
  sets up the environment, and every decision about installing or repairing is
  made before the shell starts.
- **There is a log.** Everything printed, plus the full output of every
  external command, is mirrored to `proxspace.log`.
- **`rebaseall` does not run on every start.** It is available as
  `proxspace repair --rebase`.
- **There is a way back out.** `proxspace clean` frees the download cache or
  removes the environment without touching `pm3/` and `builds/`; deleting the
  folder in Explorer used to take six months of work with it.

## Building

```
cargo build --release
```

Tests: `cargo test`. Formatting and lints: `cargo fmt`,
`cargo clippy --all-targets -- -D warnings`.

The build embeds an icon, a version resource and a manifest, which needs a
resource compiler — `rc.exe` from the Windows SDK, or `windres` from a mingw
toolchain. Without one the build still produces a working `proxspace.exe`,
prints a warning, and leaves it without them.

Three tests are marked `#[ignore]`: they download the real msys2 archive and
talk to a real mirror. Run them with `cargo test -- --ignored`.
