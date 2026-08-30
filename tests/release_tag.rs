//! The tag the release workflow publishes, against the parser that reads it
//! back.
//!
//! `release::mention_newer` asks GitHub for the newest release and compares its
//! `tag_name` with this build's version. Anything it cannot read as `x.y.z` is
//! treated as "not newer" and says nothing — which is the right way to fail,
//! and also a way to never work at all: retag the releases as `release-3.12.0`
//! and the update check goes quiet for good, with no error anywhere to notice.
//! So the two halves are pinned to each other here.

use std::io::{Cursor, Read};

use proxspace::VERSION;
use proxspace::http::{HttpClient, HttpError, Request, Response};
use proxspace::release;

const WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// GitHub, answering with one release.
struct FakeGitHub(String);

impl HttpClient for FakeGitHub {
    fn send(&self, _request: &Request) -> Result<Response, HttpError> {
        let body = format!(r#"{{"tag_name": "{}"}}"#, self.0);
        Ok(Response {
            body_len: Some(body.len() as u64),
            resumed: false,
            body: Box::new(Cursor::new(body.into_bytes())) as Box<dyn Read + Send>,
        })
    }
}

/// The tag the workflow would publish for this build, read out of the workflow
/// itself rather than assumed.
fn tag_from_the_workflow() -> String {
    let line = WORKFLOW
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("echo \"tag="))
        .expect("the workflow no longer writes a `tag` output");

    let template = line
        .trim_start_matches("echo \"tag=")
        .split('"')
        .next()
        .expect("the tag output is not a quoted string");

    assert!(
        template.contains("$version"),
        "the tag `{template}` does not come from the version"
    );
    template.replace("$version", VERSION)
}

#[test]
fn the_tag_the_workflow_publishes_is_read_as_a_version() {
    let tag = tag_from_the_workflow();
    assert_eq!(tag, format!("v{VERSION}"));

    // An older build hears about this one...
    assert_eq!(
        release::newer_than(&FakeGitHub(tag.clone()), "0.0.1").unwrap(),
        Some(tag.clone()),
        "`{tag}` is not read as a release newer than 0.0.1"
    );
    // ...and this build does not hear about itself.
    assert_eq!(
        release::newer_than(&FakeGitHub(tag), VERSION).unwrap(),
        None
    );
}

#[test]
fn the_release_is_named_after_that_tag() {
    assert!(
        WORKFLOW.contains("name: ProxSpace ${{ steps.version.outputs.tag }}"),
        "the release is no longer named `ProxSpace <tag>`"
    );
}

#[test]
fn the_version_of_this_build_has_a_changelog_section() {
    // The workflow falls back to GitHub's generated notes when the section is
    // missing, and says so only in a log nobody reads afterwards.
    assert!(
        CHANGELOG
            .lines()
            .any(|line| line.trim_end() == format!("## {VERSION}")),
        "CHANGELOG.md has no `## {VERSION}` section; the release would be published with generated notes"
    );
}
