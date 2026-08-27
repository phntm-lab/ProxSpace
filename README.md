# ProxSpace

A Proxmark3 development environment for Windows, as a single executable.

Put `proxspace.exe` in an empty folder and run it. It downloads msys2, unpacks
it next to itself, configures it, installs the toolchain needed to build and
run [Proxmark3](https://github.com/RfidResearchGroup/proxmark3), and drops you
into a shell.

## Status

Early work in progress. The command tree, paths, install state, preflight
checks and logging are in place; provisioning msys2 is not implemented yet.
Commands that are not built yet say so and exit with code 3.

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
└── proxspace.log           what happened, including external command output
```

## Commands

| Command | What it does |
|---|---|
| `proxspace` | Prepare the environment if needed, then open a shell |
| `proxspace shell [-- args]` | The same, with arguments passed to the login shell |
| `proxspace install [--force]` | Install the ProxSpace package set |
| `proxspace update [--msys2] [--packages]` | Update the environment |
| `proxspace repair [--rebase]` | Reinstall packages over a broken tree |
| `proxspace info` | Report versions, paths and toolchain state |
| `proxspace mirrors rank\|restore` | Reorder or restore pacman mirrors |
| `proxspace exec -- <cmd>` | Run one command inside the environment |
| `proxspace autobuild` | Build every Proxmark3 checkout in `pm3/` |
| `proxspace clean [--cache\|--all]` | Free disk space, or remove the environment |

Global flags: `--yes`, `--quiet`, `--verbose`, `--no-color`, `--dir <path>`.

## Requirements

- 64-bit Windows
- About 10 GB of free disk space (15 GB recommended)
- An install path made only of `A-Z a-z 0-9 . _ - \ /` — no spaces, no
  brackets, no non-ASCII characters. The msys2 toolchain and the Proxmark3
  makefiles do not survive them, and `proxspace` refuses to start rather than
  fail later in a way that is hard to diagnose.

## Differences from the original ProxSpace

This is a rewrite of [Gator96100/ProxSpace](https://github.com/Gator96100/ProxSpace),
which shipped msys2 inside the repository and drove it with `.bat` and shell
scripts. The behaviour differs deliberately in a few places:

- **msys2 is downloaded, not vendored.** The repository stays small and the
  base system is verified by checksum on the way in.
- **UCRT64 instead of MINGW64.** `MSYSTEM=UCRT64`, `PYTHONHOME=/ucrt64`.
- **The install is an explicit resumable pipeline.** The original relied on
  launching the shell twice, because `pacman -Syuu` cannot continue in a
  process whose msys2 runtime it just replaced. Each completed step is now
  recorded in `proxspace.state.json`, so an interrupted install resumes instead
  of restarting.
- **There is a log.** Everything printed, plus the full output of every
  external command, is mirrored to `proxspace.log`.
- **`rebaseall` does not run on every start.** It is available as
  `proxspace repair --rebase`.

## Building

```
cargo build --release
```

Tests: `cargo test`. Formatting and lints: `cargo fmt`,
`cargo clippy --all-targets -- -D warnings`.
