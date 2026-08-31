# Contributing

Thanks for looking. This file is the short version of how the project is put
together; the long version is the `### Development` part of
[README.md](../README.md), and the reasoning behind any single decision is in
the `//!` header of the module that carries it.

## What belongs here

ProxSpace provisions a Proxmark3 build environment and runs a shell in it. It
does not wrap the proxmark3 client, manage firmware or talk to the device, and
a change that moves it in that direction is a change to what the project is —
worth an issue first, so that nobody writes a week of code into a "no".

Everything else is welcome, especially the small things: a message that reads
wrong, a path that breaks on somebody's machine, a package that should have
been in the list.

## Before you send anything

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

These are exactly what CI runs, in the same order, and warnings are errors in
both places. `cargo test -- --ignored` adds the three tests that go out to the
network; run them if you touched downloading or mirrors.

The toolchain floor is `rust-version` in `Cargo.toml`. A change that needs a
newer compiler needs that line raised in the same commit, and a reason.

## The layers

`src/` is six layers, and a module may only name its own layer or one further
in: `core` and `ui` innermost, then `ports`, `infra`, `app`, `cli`. `core` and
`ui` are peers and neither may name the other — the decisions must not know how
they are shown, and the screen must not know what is being decided.

This is not an aspiration. `tests/layers.rs` reads the source and fails on a
path that points the wrong way, in a `use`, in an inline `crate::…` and in a
doc link alike. If it fails, the fix is almost never to move the import: it is
that the thing you needed belongs in a different layer, or that what you want
should be passed in rather than reached for.

Two rules go with it: nothing runs an external program except through
`ports::command::CommandRunner`, and nothing reaches the network except through
`ports::http::HttpClient`. Both are traits with fakes in the tests, which is
why the whole suite runs offline, in seconds.

## Comments and headers

Every module opens with a `//!` block saying what it is for, what the original
ProxSpace did in its place, and why anything that looks arbitrary is the way it
is. Add to that block when you add to the module; a change that makes a header
untrue is not finished.

Comments are in English, whatever language the discussion that produced them
was in, and they explain the technical reason rather than the history. Nothing
in a tracked file refers to a decision document, a group of work or a review
comment — the reason has to stand on its own for a reader who was not there.

## Tests

Test names are sentences about behaviour: `the_package_list_survives_verbatim`,
`a_file_another_program_holds_is_named`. A behaviour change without a test that
fails before it is not finished.

The suite touches no real msys2 tree and no network by default. If what you are
testing seems to need one, that usually means the decision and the I/O have not
been separated yet — put the decision in `core`, test it there, and let `infra`
stay thin enough to read.

## Documentation

`README.md` and `README_RU.md` are one document in two languages, and both have
to change together. Two things about them are enforced:

- The first line of each file is the language switch. `tests/docs.rs` compares
  it character for character; a badge or a heading pushed above it fails the
  build.
- The command tables under `## Commands` and `## Команды` are checked against
  the command tree clap actually builds, against each other, and against the
  per-command sections below them. Adding a command means adding a row to both
  tables, in the same position, and a `### name` section in both files.

A change that alters behaviour also wants a line in `CHANGELOG.md`, under the
section for the version in `Cargo.toml`. The release workflow publishes that
section verbatim as the release notes.

## Assets

Files under `assets/` are compiled into the binary and written into the msys2
tree at install time. They are not read from disk at runtime, so a change there
needs a rebuild to be visible, and `src/core/assets.rs` has tests that hold
them to what the tree expects: the batch files stay plain ASCII with CRLF
endings because `cmd.exe` reads them on machines that have never seen msys2,
and everything executable starts with a shebang because the mount would not
consider it executable otherwise.

## Commits and pull requests

Write the subject line as an instruction to the codebase, in one line, without
a trailing full stop: "Take the running command with us when we quit", "Make
the dependency rule a test". The body, if there is one, is prose — paragraphs
explaining why the change is the way it is, not a list of what changed. The
diff already says what changed.

Hooks are not bypassed. Nothing is committed with `--no-verify`, and a failing
check is fixed rather than skipped; if a check is wrong, that is its own
change, made deliberately and on its own.

A new dependency wants a reason in the pull request. The tree is deliberately
small and every crate in it either does something that cannot be written in an
afternoon or is already there transitively.

## Licence

Contributions are taken under the [MIT licence](../LICENSE), the same one the
rest of the project is under.
