//! Starting the login shell inside the msys2 tree.
//!
//! The original went through `msys2_shell.cmd -mingw64 -defterm -no-start`.
//! That script is two hundred lines of batch whose only job, once the terminal
//! and subsystem flags are stripped out, is to set `MSYSTEM` and run
//! `usr/bin/bash.exe -l` in the current console. Here the same three variables
//! are set directly and `bash.exe` is started as a child, which removes a layer
//! that could only ever go wrong: no `cmd.exe` in between mangling arguments,
//! no dependence on a file that lives inside the tree we manage.
//!
//! This is the one place that deliberately does not use [`CommandRunner`]:
//! that runner pipes both output streams and closes standard input, which is
//! exactly right for `pacman` and exactly wrong for a shell a human is about to
//! type into. The child inherits this console as it stands — the equivalent of
//! `-defterm -no-start` — so the shell is a normal foreground program and its
//! exit code becomes ours.
//!
//! The same goes for a single command run through [`exec`]: `pm3` talks to a
//! device and prompts for input, and a build watched through a pipe would lose
//! its progress output.
//!
//! [`CommandRunner`]: crate::command::CommandRunner

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use thiserror::Error;

use crate::interrupt;
use crate::msys2::MSYSTEM;
use crate::paths::Paths;

/// The login shell, relative to the root of the msys2 tree.
pub const BASH: &str = "usr/bin/bash.exe";

/// How `/etc/profile` is told to build `$PATH`.
///
/// `minimal` is msys2's own default, and it is set explicitly rather than left
/// to the default because the alternative is a catastrophe that only some
/// machines see: a user (or an installer) with `MSYS2_PATH_TYPE=inherit` in
/// their Windows environment gets the whole of `%PATH%` appended, and the first
/// Cygwin or second msys2 on it brings a second `msys-2.0.dll` into every
/// process the shell starts.
const PATH_TYPE: &str = "minimal";

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("`{path}` is missing; the msys2 tree is incomplete")]
    ShellMissing { path: PathBuf },
    #[error("cannot start `{path}`")]
    Spawn {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Where the login shell lives in a tree.
pub fn bash_path(tree: &Path) -> PathBuf {
    tree.join(BASH)
}

/// Variables inherited from Windows that are cleared before the shell starts.
///
/// `HOME` is the only one, and it earns its place: `/etc/profile` uses it when
/// it is set, so a `HOME` left in the user's Windows environment — Git for
/// Windows, Cygwin and several editors put one there — silently moves `$HOME`
/// off `/pm3`. The shell then starts somewhere else, `~` means something else,
/// and the proxmark3 client writes its logs into the user profile. `/etc/passwd`
/// already says where home is, and generating that file is exactly so the tree
/// can answer the question.
pub const CLEARED: &[&str] = &["HOME"];

/// Variables the login shell is started with.
///
/// Everything else — `PATH`, `PKG_CONFIG_PATH`, `HOME`, the prompt — is left to
/// `/etc/profile` and to the ProxSpace hook it sources. Presetting any of them
/// here would mean maintaining a second, silently diverging copy of what msys2
/// already computes from `MSYSTEM`.
pub fn login_env() -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("MSYSTEM"), OsString::from(MSYSTEM)),
        // Keeps `05-home-dir.post` from changing directory on us: the caller
        // has already put the process where the shell is meant to start.
        (OsString::from("CHERE_INVOKING"), OsString::from("1")),
        (OsString::from("MSYS2_PATH_TYPE"), OsString::from(PATH_TYPE)),
    ]
}

/// Arguments for the shell itself: a login shell, plus whatever was passed
/// through from the command line.
fn login_args(extra: &[String]) -> Vec<OsString> {
    let mut args = vec![OsString::from("-l")];
    args.extend(extra.iter().map(OsString::from));
    args
}

/// Where the shell starts.
///
/// Always `pm3/` — `/pm3` inside the tree, and `$HOME` as far as `/etc/passwd`
/// is concerned. Fixing it here rather than taking the console's directory is
/// what makes a double-click, a run from `PATH` and a run from some unrelated
/// folder all land in the same place. It is created if it is not there, because
/// a working directory that does not exist fails the spawn itself; the base
/// directory is the fallback, since it is the one place known to exist.
pub fn working_dir(paths: &Paths) -> PathBuf {
    let home = paths.pm3();
    if home.is_dir() || fs::create_dir_all(&home).is_ok() {
        return home;
    }
    paths.base().to_path_buf()
}

/// Arguments for one command run non-interactively.
fn exec_args(command: &[String]) -> Vec<OsString> {
    vec![
        OsString::from("-l"),
        OsString::from("-c"),
        OsString::from(script(command)),
    ]
}

/// One shell command line out of the words that followed `exec --`.
///
/// Every word is quoted, so nothing in it is read as shell syntax a second
/// time: `exec -- grep 'a b' *.c` searches for the string `a b` in a file
/// literally called `*.c`, exactly as the same words would behave if they had
/// been typed at a Windows prompt for any other program. Shell syntax is still
/// available where it is asked for explicitly — `exec -- bash -c "make | tee
/// log"` — and that is the difference worth keeping visible.
fn script(command: &[String]) -> String {
    command
        .iter()
        .map(|word| quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A single shell word, quoted if it needs it.
///
/// Single quotes, because inside them the shell interprets nothing at all; the
/// one character they cannot carry is a single quote, which is closed, escaped
/// and reopened in the usual way.
fn quote(word: &str) -> String {
    let safe =
        |character: char| character.is_ascii_alphanumeric() || "-_./:=@%+,".contains(character);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Run an interactive login shell in this console and return its exit code.
///
/// `args` goes straight to `bash -l`, so `proxspace shell -- -c "make -j"` is
/// the non-interactive form and needs no separate entry point.
pub fn run(paths: &Paths, args: &[String]) -> Result<i32, ShellError> {
    spawn(paths, login_args(args))
}

/// Run one command in the environment and return its exit code.
///
/// A login shell either way: `$PATH`, `$PYTHONHOME` and everything else the
/// toolchain needs are built by `/etc/profile` and the ProxSpace hook it
/// sources, so a command run without one would be a different environment than
/// the same command typed into `proxspace shell`.
pub fn exec(paths: &Paths, command: &[String]) -> Result<i32, ShellError> {
    spawn(paths, exec_args(command))
}

/// Start bash with the console this process was given, and wait for it.
fn spawn(paths: &Paths, args: Vec<OsString>) -> Result<i32, ShellError> {
    let bash = bash_path(&paths.msys2());
    if !bash.is_file() {
        return Err(ShellError::ShellMissing { path: bash });
    }

    let mut command = process::Command::new(&bash);
    command
        .args(args)
        .envs(login_env())
        .current_dir(working_dir(paths));
    for name in CLEARED {
        command.env_remove(name);
    }

    // For as long as the shell has the console, Ctrl+C is the user's way of
    // stopping whatever they just started in it. bash gets the signal too and
    // handles it; our own handler must not also decide the run is over.
    let _paused = interrupt::pause();

    let status = command.status().map_err(|source| ShellError::Spawn {
        path: bash.clone(),
        source,
    })?;

    // Only a process killed by a signal has no code, which cannot happen on
    // Windows; 1 is the honest answer if it ever does.
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsString> {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn the_shell_is_told_which_subsystem_it_is() {
        let env = login_env();
        assert_eq!(value_of(&env, "MSYSTEM"), Some(&OsString::from("UCRT64")));
        assert_eq!(
            value_of(&env, "MSYS2_PATH_TYPE"),
            Some(&OsString::from("minimal"))
        );
        assert!(value_of(&env, "CHERE_INVOKING").is_some());
    }

    /// `/etc/profile` builds `$PATH` from `MSYSTEM`; handing it one of our own
    /// would put the msys2 prefixes in twice, in the wrong order.
    #[test]
    fn the_path_is_left_to_the_profile() {
        let env = login_env();
        for key in ["PATH", "HOME", "PS1", "PKG_CONFIG_PATH"] {
            assert!(value_of(&env, key).is_none(), "{key} must not be preset");
        }
    }

    /// Presetting `HOME` and clearing it are not the same thing: the shell has
    /// to end up with the home `/etc/passwd` names, which happens only when
    /// nothing arrives from Windows to override it.
    #[test]
    fn the_home_inherited_from_windows_is_cleared() {
        assert!(CLEARED.contains(&"HOME"));
        assert!(value_of(&login_env(), "HOME").is_none());
    }

    #[test]
    fn no_variable_is_set_twice() {
        let env = login_env();
        let mut names: Vec<_> = env.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate variable in {env:?}");
    }

    #[test]
    fn arguments_follow_the_login_flag() {
        assert_eq!(login_args(&[]), [OsString::from("-l")]);
        assert_eq!(
            login_args(&["-c".to_string(), "make -j".to_string()]),
            ["-l", "-c", "make -j"].map(OsString::from)
        );
    }

    #[test]
    fn the_shell_starts_in_pm3() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();

        assert_eq!(working_dir(&paths), paths.pm3());
        assert!(paths.pm3().is_dir(), "the home directory must be created");
    }

    #[test]
    fn a_command_is_run_by_a_login_shell() {
        assert_eq!(
            exec_args(&["gcc".to_string(), "--version".to_string()]),
            ["-l", "-c", "gcc --version"].map(OsString::from)
        );
    }

    #[test]
    fn plain_words_are_left_as_they_are() {
        assert_eq!(quote("make"), "make");
        assert_eq!(quote("--jobs=4"), "--jobs=4");
        assert_eq!(quote("/pm3/proxmark3"), "/pm3/proxmark3");
    }

    /// Nothing that followed `exec --` may be read as shell syntax: the words
    /// were already parsed once, by our own command line.
    #[test]
    fn anything_the_shell_would_act_on_is_quoted() {
        assert_eq!(
            script(&["echo".to_string(), "a b".to_string()]),
            "echo 'a b'"
        );
        assert_eq!(quote("*.c"), "'*.c'");
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(quote("a|b"), "'a|b'");
        assert_eq!(quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn a_single_quote_is_closed_escaped_and_reopened() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn a_tree_without_a_shell_says_so_instead_of_failing_to_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();

        let error = run(&paths, &[]).unwrap_err();
        assert!(matches!(error, ShellError::ShellMissing { .. }));

        let error = exec(&paths, &["gcc".to_string()]).unwrap_err();
        assert!(matches!(error, ShellError::ShellMissing { .. }));
        assert!(error.to_string().contains("bash.exe"));
    }
}
