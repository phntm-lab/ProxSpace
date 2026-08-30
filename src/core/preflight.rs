//! Checks that must pass before the environment is touched.
//!
//! Port of the original `setup/startup_checks.sh`, which tested the install
//! path against `^[a-zA-Z0-9\/\._\-]+$` and, on failure, printed one line and
//! slept forever. The character class is kept (widened by `\` and `:`, which a
//! Windows path needs), but each rejection now says which character broke it —
//! "special characters" told the user nothing about their `C:\Program Files`.
//!
//! The restriction is not cosmetic: the msys2 toolchain, `make`, `pacman` and
//! the proxmark3 build scripts all pass this path through shell word splitting
//! and unquoted makefile variables.

/// Below this the install is refused: msys2 plus the ProxSpace package set,
/// the pacman download cache and one proxmark3 build tree do not fit.
pub const REQUIRED_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Below this the install proceeds with a warning — it fits, but leaves no room
/// for a second checkout or a firmware build.
pub const RECOMMENDED_FREE_BYTES: u64 = 15 * 1024 * 1024 * 1024;

/// Why an install path was rejected. Each variant exists to produce a message
/// the user can act on, rather than a generic "invalid path".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    Empty,
    Whitespace,
    NonAscii(char),
    Bracket(char),
    Other(char),
}

impl std::fmt::Display for PathProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathProblem::Empty => write!(f, "the path is empty"),
            PathProblem::Whitespace => write!(
                f,
                "the path contains a space; msys2 build scripts do not quote it \
                 and the build will fail in confusing ways"
            ),
            PathProblem::NonAscii(c) => write!(
                f,
                "the path contains the non-ASCII character `{c}`; the msys2 \
                 toolchain only handles ASCII paths reliably"
            ),
            PathProblem::Bracket(c) => write!(
                f,
                "the path contains `{c}`; brackets are shell metacharacters and \
                 break the build scripts"
            ),
            PathProblem::Other(c) => write!(f, "the path contains the unsupported character `{c}`"),
        }
    }
}

/// Characters an install path may consist of. Deliberately narrow: anything
/// outside this set has to survive Windows, cygwin path translation and shell
/// expansion unchanged, and most of it does not.
fn is_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '\\' | ':' | '.' | '_' | '-')
}

pub fn validate_install_path(path: &str) -> Result<(), PathProblem> {
    if path.is_empty() {
        return Err(PathProblem::Empty);
    }
    for c in path.chars() {
        if is_allowed(c) {
            continue;
        }
        return Err(match c {
            c if c.is_whitespace() => PathProblem::Whitespace,
            '(' | ')' | '[' | ']' | '{' | '}' => PathProblem::Bracket(c),
            c if !c.is_ascii() => PathProblem::NonAscii(c),
            c => PathProblem::Other(c),
        });
    }
    Ok(())
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_realistic_paths() {
        for path in [
            r"C:\ProxSpace",
            r"C:\tools\proxspace-3.11",
            r"D:\dev\pm3_env\ProxSpace",
            "/c/ProxSpace",
            r"C:\a.b\c-d\e_f",
        ] {
            assert!(validate_install_path(path).is_ok(), "rejected `{path}`");
        }
    }

    #[test]
    fn reports_the_specific_problem() {
        let cases: [(&str, PathProblem); 8] = [
            ("", PathProblem::Empty),
            (r"C:\Program Files\ProxSpace", PathProblem::Whitespace),
            ("C:\\dev\tProxSpace", PathProblem::Whitespace),
            (r"C:\Проекты\ProxSpace", PathProblem::NonAscii('П')),
            (r"C:\dev\ProxSpace (1)", PathProblem::Whitespace),
            (r"C:\dev\ProxSpace(1)", PathProblem::Bracket('(')),
            (r"C:\dev\[old]\ProxSpace", PathProblem::Bracket('[')),
            (r"C:\dev\Prox&Space", PathProblem::Other('&')),
        ];
        for (path, expected) in cases {
            assert_eq!(
                validate_install_path(path),
                Err(expected),
                "wrong verdict for `{path}`"
            );
        }
    }

    #[test]
    fn the_first_offending_character_wins() {
        // `(` comes before the space, so the bracket is reported.
        assert_eq!(
            validate_install_path(r"C:\dev\(a b)"),
            Err(PathProblem::Bracket('('))
        );
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
    }
}
