//! `info`: what this ProxSpace is, where it is, and what it is made of.
//!
//! The original `ps-info` asked Windows about itself with `wmic`, which was
//! removed in Windows 11 24H2 and now reports nothing at all; the same answers
//! come from `sysinfo` here. Everything else it printed is either already in
//! the state file or comes out of one probe run inside the environment.
//!
//! Two rules shape the module. The first is that `info` must work on a broken
//! install — that is exactly when someone runs it — so nothing here returns an
//! error: a missing tree, a shell that will not start, a tool that is not
//! installed all become a line saying so. The second is that gathering and
//! printing are separate: [`collect`] builds a [`Report`], [`Report::render`]
//! turns it into text, and the same text goes to the screen and to the file
//! next to the binary, so a report pasted into a bug report is what its author
//! saw.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::command::{Cmd, CommandRunner};
use crate::msys2::shell;
use crate::packages::PackageList;
use crate::pacman::Pacman;
use crate::paths::Paths;
use crate::state::{State, timestamp};
use crate::ui::Ui;

/// The tools the report gives a version for, and how each one is asked.
///
/// The same table builds the probe script and labels its output, so a tool
/// cannot be asked about under one name and reported under another.
const TOOLS: &[(&str, &str)] = &[
    ("arm-none-eabi-gcc", "arm-none-eabi-gcc -dumpversion"),
    ("gcc", "gcc -dumpversion"),
    ("git", "git --version"),
    ("make", "make --version"),
    ("pkgconf", "pkgconf --version"),
    ("qmake", "qmake --version"),
];

/// Label the probe reports `$PATH` under.
const PATH_LABEL: &str = "path";

/// What a field says when the environment could not be asked at all.
const UNAVAILABLE: &str = "unavailable — the environment could not be started";
/// What a field says when the environment answered but the tool is not there.
const NOT_INSTALLED: &str = "not installed";

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Heading(String),
    Field { name: String, value: String },
}

/// The report, in the order it is printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    entries: Vec<Entry>,
}

impl Report {
    pub fn new() -> Report {
        Report::default()
    }

    pub fn heading(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::Heading(text.into()));
    }

    pub fn field(&mut self, name: impl Into<String>, value: impl ToString) {
        self.entries.push(Entry::Field {
            name: name.into(),
            value: value.to_string(),
        });
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The report as text: headings flush left, fields indented and their
    /// values lined up under each other.
    ///
    /// The column is measured across the whole report rather than per section,
    /// because the alternative is a page whose values step left and right for
    /// no reason the reader can see.
    pub fn render(&self) -> String {
        let width = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Field { name, .. } => Some(name.chars().count()),
                Entry::Heading(_) => None,
            })
            .max()
            .unwrap_or(0);

        let mut text = String::new();
        for (index, entry) in self.entries.iter().enumerate() {
            match entry {
                Entry::Heading(heading) => {
                    if index > 0 {
                        text.push('\n');
                    }
                    text.push_str(heading);
                    text.push('\n');
                }
                Entry::Field { name, value } => {
                    let padding = width.saturating_sub(name.chars().count());
                    text.push_str("  ");
                    text.push_str(name);
                    text.push_str(&" ".repeat(padding + 2));
                    text.push_str(value);
                    text.push('\n');
                }
            }
        }
        text
    }
}

/// Gather the whole report.
pub fn collect(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths, state: &State) -> Report {
    let mut report = Report::new();

    report.heading("proxspace");
    report.field("version", crate::VERSION);
    report.field("generated", timestamp());
    report.field("base", paths.base().display());
    report.field("msys2", paths.msys2().display());
    report.field("home", paths.pm3().display());
    report.field("log", ui.logger().path().display());
    report.field("report", paths.info_file().display());
    report.field("state", state.stage);
    if state.was_moved_from(paths.base()) {
        report.field(
            "moved from",
            state
                .install_path
                .as_deref()
                .unwrap_or("an unrecorded path"),
        );
    }
    if state.proxspace_version != crate::VERSION {
        report.field("installed by", &state.proxspace_version);
    }

    report.heading("msys2");
    match &state.msys2 {
        Some(msys2) => {
            report.field("version", &msys2.version);
            report.field("unpacked", &msys2.extracted_at);
            report.field("source", &msys2.source_url);
        }
        None => report.field("version", NOT_INSTALLED),
    }

    report.heading("packages");
    match &state.packages {
        Some(packages) => report.field("installed", &packages.installed_at),
        None => report.field("installed", NOT_INSTALLED),
    }
    report.field(
        "python extras",
        if state.pip_extras_installed {
            "installed"
        } else {
            NOT_INSTALLED
        },
    );
    add_pins(&mut report, paths);

    report.heading("toolchain");
    add_toolchain(&mut report, runner, ui, paths);

    report.heading("system");
    add_system(&mut report);

    report
}

/// The packages held back from `pacman -Syuu`, and the version the shipped list
/// expects each of them at.
///
/// Read out of `pacman.conf` rather than remembered anywhere: the file is what
/// pacman acts on, and a pin that went missing from it is precisely the failure
/// this line exists to make visible.
fn add_pins(report: &mut Report, paths: &Paths) {
    let expected: Vec<String> = PackageList::shipped()
        .map(|list| list.pinned().map(|spec| spec.describe()).collect())
        .unwrap_or_default();

    match Pacman::new(&paths.msys2()).ignored() {
        Ok(pinned) if pinned.is_empty() => report.field("pinned", "none"),
        Ok(pinned) => report.field("pinned", pinned.join(", ")),
        Err(_) => report.field("pinned", "unreadable — there is no `pacman.conf`"),
    }
    if !expected.is_empty() {
        report.field("pin expected", expected.join(", "));
    }
}

/// `$PATH` and the tool versions, as seen from inside a login shell.
fn add_toolchain(report: &mut Report, runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) {
    let answers = probe(runner, ui, paths);

    report.field(
        "path",
        answers
            .as_ref()
            .and_then(|answers| answers.get(PATH_LABEL))
            .map(String::as_str)
            .unwrap_or(UNAVAILABLE),
    );
    for (name, _) in TOOLS {
        let value = match &answers {
            None => UNAVAILABLE,
            Some(answers) => match answers.get(*name) {
                Some(version) if !version.is_empty() => version,
                _ => NOT_INSTALLED,
            },
        };
        report.field(*name, value);
    }
}

/// Ask the environment about itself, in one login shell.
///
/// One shell rather than one per tool, because a login shell is what builds
/// `$PATH` in the first place: the versions reported are then the ones the user
/// would get by typing the same command into `proxspace shell`, and the `$PATH`
/// printed is the one that found them.
fn probe(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths) -> Option<HashMap<String, String>> {
    let bash = shell::bash_path(&paths.msys2());
    if !bash.is_file() {
        ui.detail(&format!("`{}` is not there; not probing", bash.display()));
        return None;
    }

    let output = runner
        .run(
            ui,
            &Cmd::new(&bash)
                .arg("-l")
                .arg("-c")
                .arg(probe_script())
                .envs(shell::login_env())
                .current_dir(shell::working_dir(paths))
                .label("reading the environment")
                .quiet(),
        )
        .map_err(|error| ui.detail(&format!("cannot read the environment: {error}")))
        .ok()?;

    // A shell that started but failed still prints what it managed to gather,
    // and half an answer beats none in a diagnostic.
    if !output.success() {
        ui.detail("the environment probe did not exit cleanly");
    }
    Some(parse_probe(&output.stdout))
}

/// The script the probe runs.
///
/// Each tool is asked separately and its failure is swallowed on the spot, so
/// one missing program cannot cut the answer short; only the first line of the
/// output is kept, because `make --version` and `qmake --version` go on to
/// print licence text nobody asked for.
fn probe_script() -> String {
    let mut script = format!("printf '{PATH_LABEL}=%s\\n' \"$PATH\"\n");
    for (name, command) in TOOLS {
        script.push_str(&format!(
            "printf '{name}=%s\\n' \"$({command} 2>/dev/null | head -n 1)\"\n"
        ));
    }
    script
}

/// `name=value` lines back into a table. Anything else on stdout — a warning
/// from a profile script, say — is not a label and is ignored.
fn parse_probe(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// What the machine is, for the half of the bug reports where that is the
/// answer: a 32-bit toolchain on an ARM laptop, or a build that ran out of
/// memory.
fn add_system(report: &mut Report) {
    use sysinfo::System;

    report.field(
        "os",
        System::long_os_version().unwrap_or_else(|| "unknown".to_string()),
    );
    report.field(
        "kernel",
        System::kernel_version().unwrap_or_else(|| "unknown".to_string()),
    );
    report.field("arch", System::cpu_arch());

    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let cpu = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
        .filter(|brand| !brand.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    report.field("cpu", format!("{cpu} ({} cores)", system.cpus().len()));
    report.field("memory", gibibytes(system.total_memory()));
}

/// Bytes as gibibytes, which is the unit anyone reading this thinks in.
fn gibibytes(bytes: u64) -> String {
    const GIB: f64 = (1024 * 1024 * 1024) as f64;
    format!("{:.1} GiB", bytes as f64 / GIB)
}

/// Print the report and leave a copy next to the binary.
///
/// The file is the point of the command as much as the printout is: what gets
/// pasted into an issue is a file, not a scrollback buffer. Failing to write it
/// is a warning and nothing more — the report is on screen either way.
pub fn run(runner: &dyn CommandRunner, ui: &Ui, paths: &Paths, state: &State) {
    let text = collect(runner, ui, paths, state).render();
    for line in text.lines() {
        ui.output(line);
    }
    write_report(ui, &paths.info_file(), &text);
}

fn write_report(ui: &Ui, path: &Path, text: &str) {
    match fs::write(path, text) {
        Ok(()) => ui.detail(&format!("written to `{}`", path.display())),
        Err(error) => ui.warn(&format!("cannot write `{}` ({error})", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::command::{CommandError, Output};
    use crate::logging::Logger;
    use crate::state::Stage;
    use crate::ui::UiOptions;

    fn silent_ui() -> Ui {
        Ui::new(
            UiOptions {
                quiet: true,
                ..UiOptions::default()
            },
            Arc::new(Logger::disabled()),
        )
    }

    /// A runner that must never be called: on a tree with no shell in it, the
    /// probe has to give up before running anything.
    struct NeverRuns;

    impl CommandRunner for NeverRuns {
        fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
            panic!("nothing should be run: {}", cmd.command_line());
        }
    }

    #[test]
    fn values_line_up_under_each_other() {
        let mut report = Report::new();
        report.heading("proxspace");
        report.field("version", "0.1.0");
        report.field("base", r"C:\ProxSpace");
        report.heading("system");
        report.field("os", "Windows 11");

        assert_eq!(
            report.render(),
            "proxspace\n  \
             version  0.1.0\n  \
             base     C:\\ProxSpace\n\n\
             system\n  \
             os       Windows 11\n"
        );
    }

    #[test]
    fn an_empty_report_renders_to_nothing() {
        assert_eq!(Report::new().render(), "");
    }

    #[test]
    fn the_probe_script_asks_for_every_tool_it_reports() {
        let script = probe_script();
        assert!(script.contains("$PATH"));
        for (name, command) in TOOLS {
            assert!(script.contains(command), "{command} not asked for");
            assert!(
                script.contains(&format!("{name}=%s")),
                "{name} not labelled"
            );
        }
        // One missing tool must not take the rest of the answer with it.
        assert_eq!(script.matches("2>/dev/null").count(), TOOLS.len());
    }

    #[test]
    fn labelled_lines_are_read_and_anything_else_is_not() {
        let answers = parse_probe(
            "warning: something from a profile script\n\
             path=/ucrt64/bin:/usr/bin\n\
             gcc=15.2.0\n\
             qmake=\n",
        );

        assert_eq!(answers.get("path").unwrap(), "/ucrt64/bin:/usr/bin");
        assert_eq!(answers.get("gcc").unwrap(), "15.2.0");
        assert_eq!(answers.get("qmake").unwrap(), "");
        assert_eq!(answers.len(), 3);
    }

    /// A value with an `=` in it — `qmake --version` prints one — must survive.
    #[test]
    fn only_the_first_equals_separates_a_label_from_its_value() {
        let answers = parse_probe("make=GNU Make 4.4.1 x=y\n");
        assert_eq!(answers.get("make").unwrap(), "GNU Make 4.4.1 x=y");
    }

    #[test]
    fn a_report_on_an_empty_directory_says_so_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();
        let state = State::default();

        let report = collect(&NeverRuns, &silent_ui(), &paths, &state);
        let text = report.render();

        assert!(text.contains(crate::VERSION));
        assert!(text.contains(&Stage::NotInstalled.to_string()));
        assert!(text.contains(NOT_INSTALLED), "got: {text}");
        assert!(text.contains(UNAVAILABLE), "got: {text}");
        // The system section answers even with no environment at all.
        assert!(text.contains("cpu"));
        assert!(text.contains("GiB"));
    }

    /// A tree with a shell in it, so the probe has something to run.
    struct Answers(String);

    impl CommandRunner for Answers {
        fn run(&self, _ui: &Ui, cmd: &Cmd) -> Result<Output, CommandError> {
            assert!(
                cmd.command_line().contains("bash"),
                "the probe must go through a login shell: {}",
                cmd.command_line()
            );
            Ok(Output::new(Some(0), self.0.clone(), "", cmd.describe()))
        }
    }

    fn tree_with_a_shell() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();
        fs::create_dir_all(paths.msys2().join("usr/bin")).unwrap();
        fs::write(paths.msys2().join(shell::BASH), b"not really a program").unwrap();
        (dir, paths)
    }

    /// What the environment answers is what the report says, and a tool that
    /// answered nothing is reported as missing rather than as a blank.
    #[test]
    fn the_versions_in_the_report_are_the_ones_the_environment_gave() {
        let (_dir, paths) = tree_with_a_shell();
        let answers = Answers(
            "path=/ucrt64/bin:/usr/bin\n\
             arm-none-eabi-gcc=14.2.0\n\
             gcc=15.2.0\n\
             git=git version 2.51.0\n\
             make=GNU Make 4.4.1\n\
             pkgconf=2.5.1\n\
             qmake=\n"
                .to_string(),
        );

        let text = collect(&answers, &silent_ui(), &paths, &State::default()).render();

        assert!(text.contains("/ucrt64/bin:/usr/bin"), "got: {text}");
        assert!(text.contains("git version 2.51.0"), "got: {text}");
        assert!(text.contains("arm-none-eabi-gcc  14.2.0"), "got: {text}");
        // `qmake` answered with nothing, which means it is not installed.
        assert!(text.contains(&format!("qmake              {NOT_INSTALLED}")));
        assert!(!text.contains(UNAVAILABLE), "got: {text}");
    }

    #[test]
    fn the_report_is_written_next_to_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dir(dir.path()).unwrap();

        run(&NeverRuns, &silent_ui(), &paths, &State::default());

        let written = fs::read_to_string(paths.info_file()).unwrap();
        assert!(written.contains("proxspace"), "got: {written}");
    }

    #[test]
    fn sizes_are_reported_in_the_unit_people_read_in() {
        assert_eq!(gibibytes(0), "0.0 GiB");
        assert_eq!(gibibytes(32 * 1024 * 1024 * 1024), "32.0 GiB");
    }
}
