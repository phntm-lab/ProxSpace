//! The dependency rule, checked against the source tree itself.
//!
//! The layers are an agreement, and an agreement nothing enforces lasts until
//! the first hurried import. This reads `src/`, finds every `crate::…` path in
//! it, and refuses the ones that point the wrong way.
//!
//! Doc comments count. A `[`crate::infra::…`]` link in `core` is a reference
//! from the inside out just as much as a `use` is, and it drifts for the same
//! reason.

use std::fs;
use std::path::{Path, PathBuf};

/// The layers, with how far out each one sits.
///
/// A module may name its own layer, or any layer with a strictly smaller
/// number. `core` and `ui` share the innermost rank without being the same
/// layer, which is exactly what forbids either from naming the other: the
/// decisions must not know how they are shown, and the screen must not know
/// what is being decided.
const LAYERS: &[(&str, u8)] = &[
    ("core", 0),
    ("ui", 0),
    ("ports", 1),
    ("infra", 2),
    ("app", 3),
    ("cli", 4),
];

fn rank(layer: &str) -> Option<u8> {
    LAYERS
        .iter()
        .find(|(name, _)| *name == layer)
        .map(|(_, rank)| *rank)
}

/// Whether a module in `from` may name something in `to`.
fn may_use(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match (rank(from), rank(to)) {
        (Some(from), Some(to)) => to < from,
        // Anything that is not a layer is an item at the crate root, such as
        // `crate::VERSION`, and belongs to everyone.
        _ => true,
    }
}

/// Every `crate::<name>` in the text, with the line it sits on.
fn crate_paths(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (line_number, line) in source.lines().enumerate() {
        for (offset, _) in line.match_indices("crate::") {
            // `my_crate::` and the like are not the keyword.
            let before = line[..offset].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let rest = &line[offset + "crate::".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                found.push((line_number + 1, name));
            }
        }
    }
    found
}

/// The layer a file belongs to, or `None` for `lib.rs` and `main.rs`, which
/// assemble the whole thing and may name any part of it.
fn layer_of(path: &Path, src: &Path) -> Option<String> {
    let relative = path.strip_prefix(src).ok()?;
    let first = relative.components().next()?;
    let name = first.as_os_str().to_string_lossy().to_string();
    rank(&name).map(|_| name)
}

fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("cannot read the source tree") {
        let path = entry.expect("cannot read a directory entry").path();
        if path.is_dir() {
            rust_files(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}

/// Every crossing of the rule in the tree, as a line the reader can act on.
fn violations(src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(src, &mut files);
    files.sort();

    let mut found = Vec::new();
    for file in &files {
        let Some(layer) = layer_of(file, src) else {
            continue;
        };
        let source = fs::read_to_string(file).expect("cannot read a source file");
        for (line, target) in crate_paths(&source) {
            if !may_use(&layer, &target) {
                found.push(format!(
                    "{}:{line}: `{layer}` names `crate::{target}`",
                    file.display()
                ));
            }
        }
    }
    found
}

#[test]
fn every_module_stays_inside_its_layer() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = violations(&src);
    assert!(
        found.is_empty(),
        "the dependency rule is broken in {} place(s):\n{}",
        found.len(),
        found.join("\n")
    );
}

/// Every file under `src/` has to be in a layer or be one of the two files
/// that assemble them, or it would be checked against nothing at all.
#[test]
fn nothing_lives_outside_the_layers() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let stray: Vec<String> = files
        .iter()
        .filter(|file| layer_of(file, &src).is_none())
        .filter(|file| {
            let name = file.file_name().unwrap_or_default();
            name != "lib.rs" && name != "main.rs"
        })
        .map(|file| file.display().to_string())
        .collect();

    assert!(
        stray.is_empty(),
        "these files are in no layer, so the rule says nothing about them:\n{}",
        stray.join("\n")
    );
}

/// The checker itself, against paths that do and do not break the rule. A test
/// that can only ever pass would keep passing after the rule stopped working.
#[test]
fn the_rule_lets_the_right_ones_through_and_stops_the_rest() {
    // Inwards and sideways within a layer: allowed.
    assert!(may_use("app", "core"));
    assert!(may_use("app", "infra"));
    assert!(may_use("infra", "ports"));
    assert!(may_use("ports", "core"));
    assert!(may_use("infra", "ui"));
    assert!(may_use("core", "core"));
    assert!(may_use("cli", "app"));

    // Outwards: refused.
    assert!(!may_use("core", "infra"));
    assert!(!may_use("core", "app"));
    assert!(!may_use("ports", "infra"));
    assert!(!may_use("infra", "app"));
    assert!(!may_use("app", "cli"));

    // The two innermost layers are peers and neither may name the other.
    assert!(!may_use("core", "ui"));
    assert!(!may_use("ui", "core"));
    assert!(!may_use("ui", "ports"));

    // The crate root belongs to everyone.
    assert!(may_use("core", "VERSION"));
}

/// What the scanner has to see, and what it has to leave alone.
#[test]
fn a_crate_path_is_found_wherever_it_is_written() {
    let source = "\
use crate::core::paths::Paths;
    let runner = &crate::infra::process::ProcessRunner;
/// See [`crate::ports::command`] for what a command is.
use other_crate::thing;
let text = \"crate\";
";
    let found = crate_paths(source);

    assert_eq!(
        found,
        vec![
            (1, "core".to_string()),
            (2, "infra".to_string()),
            (3, "ports".to_string()),
        ],
        "a use, an inline path and a doc link all count; another crate's name does not"
    );
}

/// A layer that has grown a new name has to be added to the table rather than
/// silently treated as an item at the crate root.
#[test]
fn the_layer_table_matches_the_directories_on_disk() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut directories: Vec<String> = fs::read_dir(&src)
        .expect("cannot read the source tree")
        .map(|entry| entry.expect("cannot read a directory entry").path())
        .filter(|path| path.is_dir())
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into()
        })
        .collect();
    directories.sort();

    let mut known: Vec<String> = LAYERS.iter().map(|(name, _)| (*name).to_string()).collect();
    known.sort();

    assert_eq!(
        directories, known,
        "the layer table and `src/` disagree about what the layers are"
    );
}
