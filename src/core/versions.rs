//! Reading and comparing the two kinds of version this binary cares about.
//!
//! msys2 names its base archives by datestamp (`20260611`) and those order as
//! plain text; ProxSpace releases are tagged `x.y.z` and have to be ordered by
//! number, or `0.10.0` would come before `0.9.0`. Both live here because both
//! answer "not newer" to anything they cannot read, and that silence is the
//! part worth having under one set of tests.

/// A msys2 version as upstream writes them: eight digits, `YYYYMMDD`.
pub fn is_datestamp(version: &str) -> bool {
    version.len() == 8 && version.bytes().all(|byte| byte.is_ascii_digit())
}

/// Last path segment of a URL, without any query string.
pub fn file_name_of(url: &str) -> &str {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url)
}

/// Datestamp out of an archive name such as
/// `msys2-base-x86_64-20260611.tar.xz`. Used to tell which base version a tree
/// came from when the state file does not say.
pub fn version_from_url(url: &str) -> Option<&str> {
    let name = file_name_of(url);
    let version = name
        .strip_prefix("msys2-base-x86_64-")?
        .strip_suffix(".tar.xz")?;
    is_datestamp(version).then_some(version)
}

/// Whether `published` names a later version than `current`.
///
/// Anything that is not a plain `x.y.z` — a tag scheme nobody expected, a
/// suffix, an empty string — is treated as "not newer". Being quiet about a
/// release that does exist is a small cost; nagging people about a release that
/// does not is a bug report.
pub fn is_newer(published: &str, current: &str) -> bool {
    match (numbers(published), numbers(current)) {
        (Some(published), Some(current)) => published > current,
        _ => false,
    }
}

/// `v1.2.3` and `1.2.3` both become `[1, 2, 3]`; anything else becomes nothing.
///
/// A trailing `-rc1` and the like are cut off rather than ordered, so a
/// pre-release never counts as newer than the release it precedes.
fn numbers(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    let version = version.split(['-', '+']).next()?;

    let parts: Vec<u64> = version
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<Vec<u64>>>()?;

    (!parts.is_empty()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_name_comes_from_the_url() {
        assert_eq!(
            file_name_of("https://example.test/a/b/msys2-base-x86_64-20260611.tar.xz"),
            "msys2-base-x86_64-20260611.tar.xz"
        );
        assert_eq!(
            file_name_of("https://example.test/get?file=msys2-base-x86_64-20260611.tar.xz"),
            "get"
        );
    }

    #[test]
    fn the_version_is_read_out_of_the_archive_name() {
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-20270115.tar.xz"),
            Some("20270115")
        );
        // Anything that is not the expected shape is not guessed at.
        assert_eq!(
            version_from_url("https://example.test/msys2-base-i686-20260611.tar.xz"),
            None
        );
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-latest.tar.xz"),
            None
        );
        assert_eq!(
            version_from_url("https://example.test/msys2-base-x86_64-20260611.tar.gz"),
            None
        );
    }

    #[test]
    fn versions_are_ordered_by_number_and_not_by_text() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.9"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("v1.2.4", "1.2.3"));
    }

    #[test]
    fn a_pre_release_is_never_newer_than_the_release_it_precedes() {
        assert!(!is_newer("0.2.0-rc1", "0.2.0"));
        assert!(is_newer("0.2.0-rc1", "0.1.0"));
    }

    #[test]
    fn an_unexpected_tag_scheme_says_nothing() {
        for tag in ["", "latest", "release-2026", "v", "1.2.x"] {
            assert!(!is_newer(tag, "0.1.0"), "{tag} was read as a version");
        }
    }

    /// Tags with fewer or more components than expected still order sensibly
    /// rather than being thrown away.
    #[test]
    fn versions_of_unequal_length_still_compare() {
        assert!(is_newer("2", "1.9.9"));
        assert!(is_newer("1.2.3.1", "1.2.3"));
        assert!(!is_newer("1.2", "1.2.1"));
    }
}
