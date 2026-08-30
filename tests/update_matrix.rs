//! The update decision, row by row.
//!
//! Three inputs decide what happens to an installed msys2 tree: the version the
//! state file records, the version this build ships, and the oldest version
//! `pacman -Syuu` can still bring all the way forward. One of the answers
//! deletes gigabytes, so the whole table is written out here rather than left
//! to a handful of examples — including the rows that are unreachable with the
//! constants this build happens to ship today.

use std::fs;

use proxspace::app::install::Plan;
use proxspace::app::update;
use proxspace::core::paths::Paths;
use proxspace::core::state::{Msys2Info, Stage, State};
use proxspace::core::update::{Reinstall, Update, decide_update};
use proxspace::infra::msys2::shell::BASH;
use proxspace::infra::msys2::{MSYS2_MIN_COMPATIBLE, MSYS2_VERSION};

/// The shape of a decision, without the version strings it carries. The
/// strings are checked on their own further down; keeping them out of the table
/// is what makes the table readable as a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    Install,
    UpToDate,
    Newer,
    Upgrade,
    Reinstall,
    Blocked,
}

fn shape(update: &Update) -> Expected {
    match update {
        Update::Install => Expected::Install,
        Update::UpToDate { .. } => Expected::UpToDate,
        Update::Newer { .. } => Expected::Newer,
        Update::Upgrade { .. } => Expected::Upgrade,
        Update::Reinstall { .. } => Expected::Reinstall,
        Update::Blocked { .. } => Expected::Blocked,
    }
}

const SHIPPED: &str = "20260611";
const MIN: &str = "20260101";

/// installed × override → decision, with `SHIPPED` and `MIN` fixed.
///
/// Read down the first column: no tree, the shipped version, a tree from a
/// newer binary, the oldest upgradable one, one in the middle, and one below
/// the floor.
#[rustfmt::skip]
const TABLE: &[(Option<&str>, Reinstall, Expected)] = &[
    (None,               Reinstall::WhenNeeded, Expected::Install),
    (None,               Reinstall::Always,     Expected::Install),
    (None,               Reinstall::Never,      Expected::Install),

    (Some("20260611"),   Reinstall::WhenNeeded, Expected::UpToDate),
    (Some("20260611"),   Reinstall::Always,     Expected::Reinstall),
    (Some("20260611"),   Reinstall::Never,      Expected::UpToDate),

    (Some("20270101"),   Reinstall::WhenNeeded, Expected::Newer),
    (Some("20270101"),   Reinstall::Always,     Expected::Reinstall),
    (Some("20270101"),   Reinstall::Never,      Expected::Newer),

    (Some("20260101"),   Reinstall::WhenNeeded, Expected::Upgrade),
    (Some("20260101"),   Reinstall::Always,     Expected::Reinstall),
    (Some("20260101"),   Reinstall::Never,      Expected::Upgrade),

    (Some("20260601"),   Reinstall::WhenNeeded, Expected::Upgrade),
    (Some("20260601"),   Reinstall::Always,     Expected::Reinstall),
    (Some("20260601"),   Reinstall::Never,      Expected::Upgrade),

    (Some("20251231"),   Reinstall::WhenNeeded, Expected::Reinstall),
    (Some("20251231"),   Reinstall::Always,     Expected::Reinstall),
    (Some("20251231"),   Reinstall::Never,      Expected::Blocked),
];

#[test]
fn every_row_of_the_matrix_decides_what_it_should() {
    for (installed, reinstall, expected) in TABLE {
        let update = decide_update(*installed, SHIPPED, MIN, *reinstall);
        assert_eq!(
            shape(&update),
            *expected,
            "installed {installed:?}, shipped {SHIPPED}, floor {MIN}, {reinstall:?} \
             produced {update:?}"
        );
    }
}

/// The floor and the shipped version being equal is not a special case in the
/// code, but it is the case this build actually ships, so it is worth its own
/// row: everything below the shipped version is then a reinstall.
#[test]
fn a_floor_at_the_shipped_version_leaves_no_room_to_upgrade() {
    for installed in ["20260610", "20200101"] {
        assert_eq!(
            shape(&decide_update(
                Some(installed),
                SHIPPED,
                SHIPPED,
                Reinstall::WhenNeeded
            )),
            Expected::Reinstall,
            "{installed} should have needed replacing"
        );
    }
}

/// Putting an older binary back next to a tree a newer one installed. There is
/// no downgrading msys2, and the tree is not broken, so nothing is undone.
#[test]
fn a_binary_rolled_back_leaves_the_tree_at_its_own_version() {
    let update = decide_update(
        Some("20270101"),
        MSYS2_VERSION,
        MSYS2_MIN_COMPATIBLE,
        Reinstall::WhenNeeded,
    );
    assert_eq!(
        update,
        Update::Newer {
            installed: "20270101".to_string(),
            shipped: MSYS2_VERSION.to_string(),
        }
    );
    assert!(!update.destroys_the_tree());
    assert!(!update.is_blocked());
}

/// Whatever a corrupted or hand-edited state file says, the answer must not be
/// the destructive one: a version nothing can be concluded from would otherwise
/// sort below every real datestamp.
#[test]
fn a_version_that_is_not_a_datestamp_is_never_read_as_ancient() {
    for installed in [
        "",
        " ",
        "unknown",
        "2026061",
        "202606110",
        "2026-06-11",
        "latest",
    ] {
        let update = decide_update(Some(installed), SHIPPED, MIN, Reinstall::WhenNeeded);
        assert_eq!(
            shape(&update),
            Expected::Upgrade,
            "{installed:?} produced {update:?}"
        );
    }
}

#[test]
fn each_decision_carries_the_versions_it_is_about() {
    assert_eq!(
        decide_update(Some("20260301"), SHIPPED, MIN, Reinstall::WhenNeeded),
        Update::Upgrade {
            from: "20260301".to_string(),
            to: SHIPPED.to_string(),
        }
    );
    assert_eq!(
        decide_update(Some("20250101"), SHIPPED, MIN, Reinstall::WhenNeeded),
        Update::Reinstall {
            from: "20250101".to_string(),
            to: SHIPPED.to_string(),
        }
    );
    assert_eq!(
        decide_update(Some("20250101"), SHIPPED, MIN, Reinstall::Never),
        Update::Blocked {
            from: "20250101".to_string(),
            to: SHIPPED.to_string(),
        }
    );
    assert_eq!(
        decide_update(Some(SHIPPED), SHIPPED, MIN, Reinstall::WhenNeeded),
        Update::UpToDate {
            version: SHIPPED.to_string(),
        }
    );
}

/// Whatever else changes, a tree installed by this build must not be found
/// wanting by this build.
#[test]
fn this_builds_own_tree_needs_nothing_doing_to_it() {
    assert_eq!(
        shape(&decide_update(
            Some(MSYS2_VERSION),
            MSYS2_VERSION,
            MSYS2_MIN_COMPATIBLE,
            Reinstall::WhenNeeded
        )),
        Expected::UpToDate
    );
}

// --- the half that looks at the disk ---

/// An installation whose state records `version`, with or without a tree to go
/// with it.
fn sandbox(version: Option<&str>, tree: bool) -> (tempfile::TempDir, Paths, State) {
    let dir = tempfile::tempdir().unwrap();
    let paths = proxspace::infra::paths::from_dir(dir.path()).unwrap();

    if tree {
        let bash = paths.msys2().join(BASH);
        fs::create_dir_all(bash.parent().unwrap()).unwrap();
        fs::write(&bash, b"not really a shell").unwrap();
    }

    let state = State {
        stage: Stage::Ready,
        msys2: version.map(|version| Msys2Info {
            version: version.to_string(),
            source_url: "https://mirror.test/msys2-base-x86_64.tar.xz".to_string(),
            sha256: "0".repeat(64),
            extracted_at: "2026-08-27T10:00:00Z".to_string(),
        }),
        ..State::default()
    };
    (dir, paths, state)
}

fn plan(paths: &Paths) -> Plan {
    let mut plan = Plan::shipped(paths).unwrap();
    plan.source.version = SHIPPED.to_string();
    plan.min_compatible = MIN.to_string();
    plan
}

#[test]
fn a_tree_and_a_state_that_agree_are_taken_at_their_word() {
    let (_dir, paths, state) = sandbox(Some("20260301"), true);
    assert_eq!(
        shape(&update::plan_update(
            &paths,
            &state,
            &plan(&paths),
            Reinstall::WhenNeeded
        )),
        Expected::Upgrade
    );
}

/// A state file left behind by a folder somebody deleted describes nothing.
#[test]
fn a_state_file_without_a_tree_is_a_fresh_install() {
    let (_dir, paths, state) = sandbox(Some("20250101"), false);
    assert_eq!(
        shape(&update::plan_update(
            &paths,
            &state,
            &plan(&paths),
            Reinstall::WhenNeeded
        )),
        Expected::Install,
        "an absent tree must not be reinstalled — there is nothing to remove"
    );
}

/// A tree no state file knows about cannot be told apart from an install that
/// stopped halfway, so it is one.
#[test]
fn a_tree_without_a_state_file_is_a_fresh_install() {
    let (_dir, paths, state) = sandbox(None, true);
    assert_eq!(
        shape(&update::plan_update(
            &paths,
            &state,
            &plan(&paths),
            Reinstall::WhenNeeded
        )),
        Expected::Install
    );
}

#[test]
fn every_decision_says_something_the_user_can_read() {
    for (installed, reinstall, _) in TABLE {
        let update = decide_update(*installed, SHIPPED, MIN, *reinstall);
        let summary = update.summary();
        assert!(!summary.is_empty(), "{update:?} has nothing to say");
        assert!(
            !summary.contains("  "),
            "{update:?} has a broken line continuation: {summary}"
        );
    }
}
