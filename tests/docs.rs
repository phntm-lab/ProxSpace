//! The command tables of the two READMEs, against the command tree itself.
//!
//! Documentation goes stale quietly; a table of commands goes stale loudly, in
//! the one place where a reader takes it for the truth and types what it says.
//! There are two of these tables now, in two languages, so they can drift from
//! the binary and from each other. These checks read both of them the way a
//! reader does — the rows of the table under the commands heading — and compare
//! what they name against what clap actually builds.

use std::collections::BTreeSet;

use clap::CommandFactory;
use proxspace::cli::args::Cli;

const ENGLISH: &str = include_str!("../README.md");
const RUSSIAN: &str = include_str!("../README_RU.md");

/// Heading each file's command table lives under.
const ENGLISH_HEADING: &str = "## Commands";
const RUSSIAN_HEADING: &str = "## Команды";

/// Every command the binary answers to, in the form the READMEs write it:
/// the empty string for a bare `proxspace`, `mirrors rank` for a subcommand of
/// a subcommand.
fn implemented() -> BTreeSet<String> {
    fn named(command: &clap::Command) -> impl Iterator<Item = &clap::Command> {
        // clap adds `help` itself; nobody documents it as a command.
        command
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
    }

    let cli = Cli::command();
    let mut commands = BTreeSet::new();
    // Running with no subcommand at all is a command like any other.
    commands.insert(String::new());

    for sub in named(&cli) {
        let name = sub.get_name();
        let mut nested = named(sub).peekable();
        if nested.peek().is_none() {
            commands.insert(name.to_string());
        } else {
            for inner in nested {
                commands.insert(format!("{name} {}", inner.get_name()));
            }
        }
    }
    commands
}

/// The rows of the command table, in the order they are written.
///
/// A checkout on Windows can hand these files over with CRLF endings, which is
/// none of this file's business: everything below reads them without.
fn documented(readme: &str, heading: &str) -> Vec<String> {
    let readme = readme.replace('\r', "");
    let section = section(&readme, heading);
    let mut commands = Vec::new();
    for line in section.lines() {
        let Some(cell) = first_cell(line) else {
            continue;
        };
        let Some(arguments) = cell.strip_prefix("proxspace") else {
            continue;
        };
        commands.extend(spellings(arguments.trim()));
    }
    assert!(
        !commands.is_empty(),
        "no command rows found under `{heading}`"
    );
    commands
}

/// Everything between a heading and the next one of the same level.
fn section<'a>(readme: &'a str, heading: &str) -> &'a str {
    let start = readme
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("no `{heading}` section"));
    let rest = &readme[start + 1..];
    match rest[heading.len()..].find("\n## ") {
        Some(end) => &rest[..heading.len() + end],
        None => rest,
    }
}

/// The backticked contents of a table row's first cell, if it has any. A cell
/// can contain a pipe of its own as long as it is escaped, which is how the
/// rows spell alternatives.
fn first_cell(line: &str) -> Option<&str> {
    let row = line.strip_prefix("| ")?;
    let end = row
        .char_indices()
        .find(|&(at, character)| character == '|' && !row[..at].ends_with('\\'))
        .map_or(row.len(), |(at, _)| at);
    let cell = row[..end].trim();
    cell.strip_prefix('`')?.strip_suffix('`')
}

/// The commands one row stands for. A row names alternatives with an escaped
/// pipe (`mirrors rank\|restore`) and optional parts in brackets, which are
/// arguments rather than commands.
fn spellings(arguments: &str) -> Vec<String> {
    let words: Vec<&str> = arguments
        .split_whitespace()
        .take_while(|word| !word.starts_with('[') && !word.starts_with('<') && *word != "--")
        .collect();

    match words.split_last() {
        None => vec![String::new()],
        Some((last, leading)) => last
            .split("\\|")
            .map(|alternative| {
                leading
                    .iter()
                    .chain(std::iter::once(&alternative))
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect(),
    }
}

#[test]
fn the_english_table_names_every_command_and_nothing_else() {
    let documented: BTreeSet<String> = documented(ENGLISH, ENGLISH_HEADING).into_iter().collect();
    assert_eq!(documented, implemented());
}

#[test]
fn the_russian_table_names_every_command_and_nothing_else() {
    let documented: BTreeSet<String> = documented(RUSSIAN, RUSSIAN_HEADING).into_iter().collect();
    assert_eq!(documented, implemented());
}

#[test]
fn the_two_tables_are_the_same_table_in_two_languages() {
    assert_eq!(
        documented(ENGLISH, ENGLISH_HEADING),
        documented(RUSSIAN, RUSSIAN_HEADING),
        "the command tables list different commands, or list them in a different order"
    );
}

#[test]
fn each_command_of_the_table_has_a_section_of_its_own() {
    // `mirrors rank` and `mirrors restore` are described under `mirrors`, and
    // a bare `proxspace` under `shell`, which is what it runs.
    for (readme, heading, language) in [
        (ENGLISH, ENGLISH_HEADING, "English"),
        (RUSSIAN, RUSSIAN_HEADING, "Russian"),
    ] {
        for command in documented(readme, heading) {
            let name = command.split_whitespace().next().unwrap_or("shell");
            assert!(
                readme
                    .lines()
                    .any(|line| line.trim_end() == format!("### {name}")),
                "the {language} README has no `### {name}` section"
            );
        }
    }
}

#[test]
fn both_readmes_open_with_the_language_switch() {
    let first = |readme: &str| {
        readme
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_string()
    };
    assert_eq!(first(ENGLISH), "**English** · [Русский](README_RU.md)");
    assert_eq!(first(RUSSIAN), "[English](README.md) · **Русский**");
}
