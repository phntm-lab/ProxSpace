# Changelog

Notable changes, newest first. Each heading is the version in `Cargo.toml`; the
release workflow publishes the section under it as the release notes.

## 3.12.0

The rewrite: ProxSpace is now a single executable instead of a repository with
msys2 vendored inside it and a set of `.bat` and shell scripts around it.

### Added

- One binary with a command tree — `shell`, `install`, `update`, `repair`,
  `info`, `mirrors rank|restore`, `exec`, `autobuild`, `clean` — replacing
  `runme64.bat`, `autobuild.bat`, `setup.cmd` and `setup/bin/*`. The old names
  live on as shims inside the shell.
- A resumable install pipeline: every completed step is recorded in
  `proxspace.state.json`, so an interrupted install carries on instead of
  starting over.
- Ctrl+C stops at the end of the step that is running, leaving the install
  resumable and a partly downloaded archive to continue from; a second one
  quits at once and takes the running command with it, so nothing is left
  behind holding the package database.
- `proxspace info`, which reports versions, paths, the toolchain and the
  machine, and writes the same text to `proxspace-info.txt` for bug reports. It
  is the one command that works on a broken or missing install.
- `proxspace clean`, which frees the pacman cache or removes the environment
  without touching `pm3/` and `builds/`.
- `proxspace mirrors rank|restore`, measuring pacman mirrors and reordering
  them fastest first.
- A log: everything printed, plus the full output of every external command,
  is mirrored to `proxspace.log` and rotated past 5 MB.
- Preflight checks before anything is touched, including the install path — a
  space or a bracket in it is refused with the offending character named,
  rather than failing later inside `make`.
- An icon, version information and a manifest on the executable.

### Changed

- msys2 is downloaded from `mirror.msys2.org` and verified against a sha256
  built into the binary, instead of being vendored in the repository.
- UCRT64 replaces MINGW64: `MSYSTEM=UCRT64`, `PYTHONHOME=/ucrt64` and the
  `mingw-w64-ucrt-x86_64-*` packages.
- Nothing is installed from inside a login shell any more; the login hook only
  sets the environment up.
- `rebaseall` no longer runs on every start. It is available as
  `proxspace repair --rebase` for the one symptom it fixes.
- Everything lives next to the executable and nothing is written to
  `%APPDATA%`, `%TEMP%` or the user profile, so the folder can be moved or
  copied whole.
