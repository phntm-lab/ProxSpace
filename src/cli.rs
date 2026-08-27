//! Command tree.
//!
//! Replaces `runme64.bat`, `autobuild.bat`, `setup/setup.cmd` and every script
//! in `setup/bin/` with subcommands of one binary. Running with no arguments is
//! the `runme64.bat` case — prepare the environment, then hand the user a
//! shell — because that is what most people double-click the executable for.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Exit code for a subcommand that exists but is not implemented yet.
/// Distinct from 1 so that scripts can tell "this failed" from "this is not
/// built yet". Usage errors are clap's own exit code 2.
pub const EXIT_NOT_IMPLEMENTED: i32 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "proxspace",
    version,
    about = "Proxmark3 development environment for Windows",
    long_about = "Downloads, configures and runs a self-contained msys2/UCRT64 \
                  environment for building and using Proxmark3.\n\n\
                  Everything is created next to this executable: the msys2 tree, \
                  the pm3 home directory, the state file and the log."
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args, Clone, Default)]
pub struct GlobalArgs {
    /// Answer yes to every confirmation (for unattended runs)
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// Print only warnings, errors and command output
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Print the detail that normally only goes to the log
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Never colourise output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Use this directory instead of the one holding the executable
    #[arg(long, global = true, value_name = "PATH")]
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Prepare the environment if needed, then start an interactive shell
    Shell {
        /// Arguments passed straight through to the login shell
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Install the ProxSpace package set
    Install {
        /// Reinstall every package even if it is already present
        #[arg(long)]
        force: bool,
    },

    /// Update the environment
    Update {
        /// Update the msys2 base system only
        #[arg(long)]
        msys2: bool,
        /// Update the ProxSpace package set only
        #[arg(long)]
        packages: bool,
    },

    /// Reinstall every installed package on top of itself to fix a broken tree
    Repair {
        /// Also rebase the msys2 DLLs (slow; only helps with fork() failures)
        #[arg(long)]
        rebase: bool,
    },

    /// Report versions, paths and toolchain state
    Info,

    /// Manage pacman mirror ordering
    Mirrors {
        #[command(subcommand)]
        action: MirrorsAction,
    },

    /// Run one command inside the environment and exit
    Exec {
        /// The command and its arguments
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },

    /// Build every proxmark3 checkout found in pm3/
    Autobuild,

    /// Free disk space, or remove the environment entirely
    Clean {
        /// Remove the msys2 tree; pm3/ and builds/ are never touched
        #[arg(long, conflicts_with = "cache")]
        all: bool,
        /// Remove downloaded packages from the pacman cache (the default)
        #[arg(long)]
        cache: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum MirrorsAction {
    /// Reorder mirrors by measured speed
    Rank,
    /// Restore the mirror lists shipped with msys2
    Restore,
}

impl Command {
    /// Human-readable name used in messages and in the log.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Shell { .. } => "shell",
            Command::Install { .. } => "install",
            Command::Update { .. } => "update",
            Command::Repair { .. } => "repair",
            Command::Info => "info",
            Command::Mirrors { action } => match action {
                MirrorsAction::Rank => "mirrors rank",
                MirrorsAction::Restore => "mirrors restore",
            },
            Command::Exec { .. } => "exec",
            Command::Autobuild => "autobuild",
            Command::Clean { .. } => "clean",
        }
    }

    /// Whether the command operates on the environment and therefore has to
    /// pass preflight first. `info` is excluded on purpose: diagnosing a broken
    /// install is exactly when the checks fail, and refusing to report is the
    /// least helpful possible response.
    pub fn needs_preflight(&self) -> bool {
        !matches!(self, Command::Info)
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_means_no_subcommand() {
        let cli = Cli::try_parse_from(["proxspace"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn global_flags_work_before_and_after_the_subcommand() {
        for args in [
            vec!["proxspace", "--yes", "install"],
            vec!["proxspace", "install", "--yes"],
        ] {
            let cli = Cli::try_parse_from(args.clone()).unwrap();
            assert!(cli.global.yes, "--yes lost in {args:?}");
        }
    }

    #[test]
    fn shell_arguments_are_passed_through_after_a_separator() {
        let cli = Cli::try_parse_from(["proxspace", "shell", "--", "-c", "make -j"]).unwrap();
        match cli.command {
            Some(Command::Shell { args }) => assert_eq!(args, ["-c", "make -j"]),
            other => panic!("expected shell, got {other:?}"),
        }
    }

    #[test]
    fn exec_requires_a_command() {
        assert!(Cli::try_parse_from(["proxspace", "exec"]).is_err());
        let cli = Cli::try_parse_from(["proxspace", "exec", "--", "gcc", "--version"]).unwrap();
        match cli.command {
            Some(Command::Exec { command }) => assert_eq!(command, ["gcc", "--version"]),
            other => panic!("expected exec, got {other:?}"),
        }
    }

    #[test]
    fn quiet_and_verbose_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["proxspace", "--quiet", "--verbose", "info"]).is_err());
    }

    #[test]
    fn clean_all_and_clean_cache_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["proxspace", "clean", "--all", "--cache"]).is_err());
    }

    #[test]
    fn mirror_actions_are_named() {
        let cli = Cli::try_parse_from(["proxspace", "mirrors", "rank"]).unwrap();
        assert_eq!(cli.command.unwrap().name(), "mirrors rank");
    }

    #[test]
    fn only_info_skips_preflight() {
        assert!(!Command::Info.needs_preflight());
        assert!(Command::Autobuild.needs_preflight());
        assert!(Command::Shell { args: vec![] }.needs_preflight());
    }
}
