//! The package list: what the environment is made of.
//!
//! The original kept this in `setup/packages.txt` and read it with
//! `grep "^[^#;]" | for pkg in ...`. The file is still the source of truth and
//! is still shipped verbatim into the tree so that the user can read it, but it
//! is compiled into the binary (`assets::PACKAGES`) and parsed here rather than
//! being looked for on disk — a list that can go missing is a list that can
//! silently install nothing.
//!
//! One thing the original list did not have is a line with a URL. The pinned
//! `arm-none-eabi-binutils` is installed from a fixed `.pkg.tar.zst` because the
//! repository version breaks the proxmark3 build, and everything about handling
//! it — is it installed? which version? does it need reinstalling after an
//! upgrade? — needs the package name and version, which exist only inside the
//! file name in that URL. Digging them out is what most of this module is
//! about.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Suffix every pacman package file carries. Everything before it is
/// `name-version-release-arch`.
const PACKAGE_SUFFIX: &str = ".pkg.tar";

/// Characters pacman allows in a package name — alphanumerics plus these.
/// Anything else in the list is a typo, and it is far better to hear about it
/// here than from pacman in the middle of an install.
const NAME_PUNCTUATION: &[char] = &['@', '.', '_', '+', '-'];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackagesError {
    #[error("package list, line {line}: `{text}` has whitespace in it; write one package per line")]
    Whitespace { line: usize, text: String },
    #[error("package list, line {line}: `{text}` is not a valid package name")]
    BadName { line: usize, text: String },
    #[error(
        "package list, line {line}: `{url}` is not a package URL; \
         it has to end in a file name like `name-1.2.3-1-any.pkg.tar.zst`"
    )]
    BadUrl { line: usize, url: String },
    #[error("package list, line {line}: only http and https URLs can be installed, not `{url}`")]
    BadScheme { line: usize, url: String },
    #[error("package list, line {line}: `{name}` is listed twice (already on line {first})")]
    Duplicate {
        line: usize,
        first: usize,
        name: String,
    },
    #[error("the package list is empty")]
    Empty,
}

/// One entry of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgSpec {
    /// Installed by name from the repositories, with `pacman -S`.
    Name(String),
    /// Installed from a fixed file, with `pacman -U`, and pinned afterwards so
    /// that `pacman -Syuu` cannot pull it forward again.
    Url {
        url: String,
        name: String,
        /// `pkgver-pkgrel`, in the form `pacman -Q` prints it, so the two can
        /// be compared without further parsing.
        version: String,
    },
}

impl PkgSpec {
    /// The package name, whichever way the package arrives.
    pub fn name(&self) -> &str {
        match self {
            PkgSpec::Name(name) => name,
            PkgSpec::Url { name, .. } => name,
        }
    }

    /// The version this list demands, when it demands one. A repository package
    /// has no expected version: whatever the repository holds is correct.
    pub fn version(&self) -> Option<&str> {
        match self {
            PkgSpec::Name(_) => None,
            PkgSpec::Url { version, .. } => Some(version),
        }
    }

    pub fn is_pinned(&self) -> bool {
        matches!(self, PkgSpec::Url { .. })
    }

    /// How the entry is named in messages: the name, plus the pinned version
    /// when there is one, so that a line about it says why it is special.
    pub fn describe(&self) -> String {
        match self {
            PkgSpec::Name(name) => name.clone(),
            PkgSpec::Url { name, version, .. } => format!("{name} {version} (pinned)"),
        }
    }

    /// The canonical line this entry came from, and the only thing hashed into
    /// [`PackageList::list_hash`].
    fn canonical(&self) -> &str {
        match self {
            PkgSpec::Name(name) => name,
            PkgSpec::Url { url, .. } => url,
        }
    }
}

/// The whole list, in the order it was written.
///
/// Order matters less than it looks — pacman resolves dependencies itself and
/// the packages are installed in one batch — but it is the order the user sees
/// in progress messages, and keeping it makes the list and the output match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageList {
    specs: Vec<PkgSpec>,
}

impl PackageList {
    /// The list this build ships.
    ///
    /// Parsing cannot realistically fail — the list is a compiled-in constant
    /// and a test parses it — but it is still a `Result` rather than a panic,
    /// because the failure mode of a wrong list is an install that stops with a
    /// message, not a binary that dies.
    pub fn shipped() -> Result<PackageList, PackagesError> {
        PackageList::parse(crate::assets::PACKAGES)
    }

    /// Read a package list.
    ///
    /// Follows `grep "^[^#;]"` in what it skips — blank lines, and lines
    /// starting with `#` or `;` — and diverges from it in two deliberate ways:
    ///
    /// - the comment marker is looked for after leading whitespace, so an
    ///   indented `# ...` is a comment rather than a package named `#`;
    /// - a `#` or `;` *after* whitespace ends the line, which lets an entry
    ///   carry a note about why it is there. The original's word splitting
    ///   would have turned such a note into three more packages.
    pub fn parse(text: &str) -> Result<PackageList, PackagesError> {
        let mut specs = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let Some(entry) = strip_comment(raw) else {
                continue;
            };
            if entry.split_whitespace().count() > 1 {
                return Err(PackagesError::Whitespace {
                    line,
                    text: entry.to_string(),
                });
            }

            let spec = parse_entry(entry, line)?;
            if let Some(first) = seen.insert(spec.name().to_string(), line) {
                return Err(PackagesError::Duplicate {
                    line,
                    first,
                    name: spec.name().to_string(),
                });
            }
            specs.push(spec);
        }

        if specs.is_empty() {
            return Err(PackagesError::Empty);
        }
        Ok(PackageList { specs })
    }

    pub fn specs(&self) -> &[PkgSpec] {
        &self.specs
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Every package name in the list, however it is installed. This is the set
    /// that has to be present for the environment to be complete.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.specs.iter().map(PkgSpec::name)
    }

    /// The names installed from the repositories, which go to `pacman -S` in
    /// one batch: one transaction resolves the dependencies between them once,
    /// where the original's package-at-a-time loop resolved them sixty times.
    pub fn repo_names(&self) -> Vec<&str> {
        self.specs
            .iter()
            .filter(|spec| !spec.is_pinned())
            .map(PkgSpec::name)
            .collect()
    }

    /// The entries installed from a URL and held at that version.
    pub fn pinned(&self) -> impl Iterator<Item = &PkgSpec> {
        self.specs.iter().filter(|spec| spec.is_pinned())
    }

    /// Fingerprint of the list, recorded in `state.json` so that a later build
    /// shipping a different list can install the difference instead of either
    /// ignoring it or reinstalling everything.
    ///
    /// Computed over the parsed entries rather than the file text: editing a
    /// comment or a section banner changes the file but not what gets
    /// installed, and it must not cost the user a package check.
    pub fn list_hash(&self) -> String {
        let mut hasher = Sha256::new();
        for spec in &self.specs {
            hasher.update(spec.canonical().as_bytes());
            hasher.update(b"\n");
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// The content of one line, or `None` if the line holds nothing to install.
fn strip_comment(raw: &str) -> Option<&str> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with(['#', ';']) {
        return None;
    }
    // A marker only ends the line when it follows whitespace: `#` is not legal
    // in a package name, but it is legal in a URL, and guessing wrong there
    // would silently install the wrong file.
    let entry = match line.find([' ', '\t']) {
        Some(space) if line[space..].trim_start().starts_with(['#', ';']) => line[..space].trim(),
        _ => line,
    };
    (!entry.is_empty()).then_some(entry)
}

fn parse_entry(entry: &str, line: usize) -> Result<PkgSpec, PackagesError> {
    if let Some(position) = entry.find("://") {
        let scheme = &entry[..position];
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(PackagesError::BadScheme {
                line,
                url: entry.to_string(),
            });
        }
        let (name, version) =
            split_package_file(file_name_of(entry)).ok_or_else(|| PackagesError::BadUrl {
                line,
                url: entry.to_string(),
            })?;
        return Ok(PkgSpec::Url {
            url: entry.to_string(),
            name,
            version,
        });
    }

    if !is_package_name(entry) {
        return Err(PackagesError::BadName {
            line,
            text: entry.to_string(),
        });
    }
    Ok(PkgSpec::Name(entry.to_string()))
}

fn is_package_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || NAME_PUNCTUATION.contains(&c))
}

/// Last path segment of a URL, without any query string or fragment.
fn file_name_of(url: &str) -> &str {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url)
}

/// Take a package file name apart into `(name, version)`.
///
/// The shape is `name-pkgver-pkgrel-arch.pkg.tar.<compression>`, and it is read
/// from the right because only the right-hand end is fixed: the name itself
/// contains as many dashes as it likes
/// (`mingw-w64-ucrt-x86_64-arm-none-eabi-binutils`), while `pkgver` may not
/// contain one at all — that is what makes the split unambiguous.
///
/// The version comes back as `pkgver-pkgrel`, which is exactly what
/// `pacman -Q` prints, so comparing the two needs no further work.
pub fn split_package_file(file_name: &str) -> Option<(String, String)> {
    let stem = &file_name[..file_name.find(PACKAGE_SUFFIX)?];

    // From the right: arch, pkgrel, pkgver, and whatever is left is the name.
    let (rest, arch) = stem.rsplit_once('-')?;
    let (rest, release) = rest.rsplit_once('-')?;
    let (name, version) = rest.rsplit_once('-')?;

    let plausible = !name.is_empty()
        && !arch.is_empty()
        && version.starts_with(|c: char| c.is_ascii_alphanumeric())
        // pacman requires pkgrel to be a number, possibly with a sub-release
        // (`1`, `1.1`). Checking it is what stops a name like `foo-bar-baz`
        // from being read as a version.
        && release.starts_with(|c: char| c.is_ascii_digit());
    plausible.then(|| (name.to_string(), format!("{version}-{release}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The URL that made all of this necessary.
    const BINUTILS_URL: &str = "https://repo.msys2.org/mingw/ucrt64/\
         mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.46.1-1-any.pkg.tar.zst";

    #[test]
    fn the_shipped_list_parses_and_contains_what_it_should() {
        let list = PackageList::shipped().unwrap();

        assert!(list.len() > 20, "the shipped list looks truncated");
        assert!(list.names().any(|name| name == "git"));
        assert!(list.names().any(|name| name == "base-devel"));
        assert!(
            list.names()
                .any(|name| name == "mingw-w64-ucrt-x86_64-qt6-base")
        );

        // The commented-out ChameleonMini section must stay commented out.
        assert!(!list.names().any(|name| name.contains("avrdude")));
        assert!(!list.names().any(|name| name.contains("dfu-programmer")));
    }

    #[test]
    fn the_shipped_list_pins_binutils_and_only_binutils() {
        let list = PackageList::shipped().unwrap();
        let pinned: Vec<&PkgSpec> = list.pinned().collect();

        assert_eq!(pinned.len(), 1, "got: {pinned:?}");
        assert_eq!(
            pinned[0].name(),
            "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils"
        );
        assert_eq!(pinned[0].version(), Some("2.46.1-1"));

        // And the pinned one is not also in the batch installed by name, which
        // would undo the pin on the spot.
        assert!(!list.repo_names().contains(&pinned[0].name()));
        assert_eq!(list.repo_names().len(), list.len() - 1);
    }

    #[test]
    fn comments_blank_lines_and_banners_are_skipped() {
        let list = PackageList::parse(
            "\
##############################
#         General            #
##############################

git
   ; another comment style
   # an indented comment, which `grep \"^[^#;]\"` would have taken
make
",
        )
        .unwrap();

        assert_eq!(
            list.specs(),
            &[
                PkgSpec::Name("git".to_string()),
                PkgSpec::Name("make".to_string())
            ]
        );
    }

    #[test]
    fn a_note_after_an_entry_is_not_a_package() {
        let list =
            PackageList::parse("lua  # needed by the pm3 scripts\nmake\t; and this\n").unwrap();
        assert_eq!(
            list.specs(),
            &[
                PkgSpec::Name("lua".to_string()),
                PkgSpec::Name("make".to_string())
            ]
        );
    }

    #[test]
    fn a_url_yields_its_name_and_version() {
        let list = PackageList::parse(BINUTILS_URL).unwrap();
        assert_eq!(
            list.specs(),
            &[PkgSpec::Url {
                url: BINUTILS_URL.to_string(),
                name: "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils".to_string(),
                version: "2.46.1-1".to_string(),
            }]
        );
        assert!(list.specs()[0].is_pinned());
        assert_eq!(
            list.specs()[0].describe(),
            "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils 2.46.1-1 (pinned)"
        );
    }

    #[test]
    fn package_file_names_split_at_the_right_dashes() {
        assert_eq!(
            split_package_file(
                "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils-2.46.1-1-any.pkg.tar.zst"
            ),
            Some((
                "mingw-w64-ucrt-x86_64-arm-none-eabi-binutils".to_string(),
                "2.46.1-1".to_string()
            ))
        );
        // Older compression, and a sub-release, both still occur in the wild.
        assert_eq!(
            split_package_file("bash-5.2.037-2.1-x86_64.pkg.tar.xz"),
            Some(("bash".to_string(), "5.2.037-2.1".to_string()))
        );
        // An epoch is part of pkgver and stays with it.
        assert_eq!(
            split_package_file("gnupg-2:2.4.5-1-x86_64.pkg.tar.zst"),
            Some(("gnupg".to_string(), "2:2.4.5-1".to_string()))
        );
    }

    #[test]
    fn what_is_not_a_package_file_name_is_not_guessed_at() {
        assert_eq!(split_package_file("index.html"), None);
        assert_eq!(split_package_file("bash.pkg.tar.zst"), None);
        // No pkgrel: the last three fields have to look like version, release,
        // arch, and `none` does not look like a release.
        assert_eq!(split_package_file("arm-none-eabi-gcc.pkg.tar.zst"), None);
        assert_eq!(split_package_file("a-b-none-any.pkg.tar.zst"), None);
    }

    #[test]
    fn a_url_that_is_not_a_package_is_refused_with_its_line_number() {
        let error = PackageList::parse("git\nhttps://example.test/latest\n").unwrap_err();
        assert_eq!(
            error,
            PackagesError::BadUrl {
                line: 2,
                url: "https://example.test/latest".to_string()
            }
        );
        assert!(error.to_string().contains("name-1.2.3-1-any.pkg.tar.zst"));
    }

    #[test]
    fn only_http_urls_can_be_installed() {
        let error = PackageList::parse("ftp://example.test/a-1-1-any.pkg.tar.zst").unwrap_err();
        assert!(matches!(error, PackagesError::BadScheme { line: 1, .. }));
    }

    #[test]
    fn a_name_with_nonsense_in_it_is_refused() {
        let error = PackageList::parse("git\nmingw-*\n").unwrap_err();
        assert_eq!(
            error,
            PackagesError::BadName {
                line: 2,
                text: "mingw-*".to_string()
            }
        );
    }

    #[test]
    fn two_packages_on_one_line_are_refused() {
        // The original's `for pkg in $(grep ...)` split these into two; here it
        // is a mistake in the list, and the list is ours to fix.
        let error = PackageList::parse("git make\n").unwrap_err();
        assert!(matches!(error, PackagesError::Whitespace { line: 1, .. }));
    }

    #[test]
    fn the_same_package_twice_is_refused_including_as_a_pin() {
        let error = PackageList::parse("git\nmake\ngit\n").unwrap_err();
        assert_eq!(
            error,
            PackagesError::Duplicate {
                line: 3,
                first: 1,
                name: "git".to_string()
            }
        );

        // The case that actually matters: the pinned package also listed by
        // name would be pulled forward by the batch install and lose its pin.
        let list = format!("mingw-w64-ucrt-x86_64-arm-none-eabi-binutils\n{BINUTILS_URL}\n");
        assert!(matches!(
            PackageList::parse(&list).unwrap_err(),
            PackagesError::Duplicate {
                line: 2,
                first: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_list_with_nothing_in_it_is_an_error_not_an_empty_install() {
        assert_eq!(
            PackageList::parse("# everything is commented out\n").unwrap_err(),
            PackagesError::Empty
        );
        assert_eq!(PackageList::parse("").unwrap_err(), PackagesError::Empty);
    }

    #[test]
    fn the_hash_follows_the_packages_and_not_the_formatting() {
        let plain = PackageList::parse("git\nmake\n").unwrap();
        let decorated = PackageList::parse(
            "\
# ==== General ====

git

  # a note
make
",
        )
        .unwrap();
        assert_eq!(plain.list_hash(), decorated.list_hash());

        let changed = PackageList::parse("git\nmake\npkgconf\n").unwrap();
        assert_ne!(plain.list_hash(), changed.list_hash());

        // Order is part of the identity: a reordered list is a different list
        // only in so far as the hash says so, and saying so costs one package
        // check, while missing a real change costs a broken environment.
        let reordered = PackageList::parse("make\ngit\n").unwrap();
        assert_ne!(plain.list_hash(), reordered.list_hash());

        assert_eq!(plain.list_hash().len(), 64);
    }

    #[test]
    fn the_hash_notices_a_pinned_version_changing() {
        let before = PackageList::parse(BINUTILS_URL).unwrap();
        let after = PackageList::parse(&BINUTILS_URL.replace("2.46.1-1", "2.47.0-1")).unwrap();
        assert_ne!(before.list_hash(), after.list_hash());
    }
}
