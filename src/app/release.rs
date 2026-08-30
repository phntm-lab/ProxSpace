//! Whether a newer ProxSpace has been published.
//!
//! A courtesy, and nothing more. The binary never replaces itself: an
//! executable that overwrites itself while running is a Windows problem with no
//! good answer, and one that would have to be got right on a machine where the
//! only recovery is a manual download anyway. So this asks GitHub what the
//! newest release is and, if it is newer than this build, says so with a link.
//!
//! Every failure here is silent. There is no network on a lot of the machines
//! this runs on, GitHub rate-limits unauthenticated callers, and a release
//! check has no business turning a working `update` into a failed one — so a
//! failed check goes to the log and nowhere else.

use std::io::Read;

use serde::Deserialize;

use crate::VERSION;
use crate::core::versions::is_newer;
use crate::ports::http::{HttpClient, Request};
use crate::ui::Ui;

/// The unauthenticated releases endpoint. It excludes pre-releases and drafts,
/// which is what makes it the right one to nag people with.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/phntm-lab/ProxSpace/releases/latest";

/// Where the user is sent to get it.
pub const RELEASES_PAGE: &str = "https://github.com/phntm-lab/ProxSpace/releases/latest";

/// How much of the answer is read. The interesting field is in the first few
/// hundred bytes; the rest is release notes, and a body without an end is not
/// going to become one.
const MAX_RESPONSE: u64 = 64 * 1024;

/// The one field of the GitHub release that is used. Everything else in that
/// JSON — notes, assets, authors — is somebody else's business.
#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
}

/// Say that a newer ProxSpace exists, if one does.
///
/// Never fails and never gets in the way: a check that cannot be made is a
/// line in the log.
pub fn mention_newer(http: &dyn HttpClient, ui: &Ui) {
    match newer_than(http, VERSION) {
        Ok(Some(version)) => {
            ui.info(&format!(
                "ProxSpace {version} has been released; this is {VERSION}"
            ));
            ui.info(&format!("get it from {RELEASES_PAGE}"));
        }
        Ok(None) => ui.detail("this is the newest published ProxSpace"),
        Err(reason) => ui.detail(&format!("could not check for a newer ProxSpace: {reason}")),
    }
}

/// The published version, when it is newer than `current`.
///
/// The error is a sentence for the log rather than a type: nothing can act on
/// the difference between "no network" and "GitHub is rate-limiting us".
pub fn newer_than(http: &dyn HttpClient, current: &str) -> Result<Option<String>, String> {
    let response = http
        .send(&Request::get(LATEST_RELEASE_API))
        .map_err(|error| error.to_string())?;

    let mut body = Vec::new();
    response
        .body
        .take(MAX_RESPONSE)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;

    let release: LatestRelease =
        serde_json::from_slice(&body).map_err(|error| format!("unexpected answer ({error})"))?;

    let published = release.tag_name.trim().to_string();
    Ok(is_newer(&published, current).then_some(published))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::ports::http::{HttpError, Response};

    /// Answers with a fixed body, or refuses to answer at all.
    struct FakeGitHub(Result<String, ()>);

    impl HttpClient for FakeGitHub {
        fn send(&self, request: &Request) -> Result<Response, HttpError> {
            match &self.0 {
                Ok(body) => Ok(Response {
                    body_len: Some(body.len() as u64),
                    resumed: false,
                    body: Box::new(Cursor::new(body.clone().into_bytes())),
                }),
                Err(()) => Err(HttpError::Transport {
                    url: request.url.clone(),
                    source: std::io::Error::other("the network is down"),
                }),
            }
        }
    }

    fn serving(tag: &str) -> FakeGitHub {
        FakeGitHub(Ok(format!(
            r#"{{"tag_name": "{tag}", "html_url": "https://example.test/r", "body": "notes"}}"#
        )))
    }

    #[test]
    fn a_later_release_is_reported() {
        assert_eq!(
            newer_than(&serving("v0.2.0"), "0.1.0").unwrap(),
            Some("v0.2.0".to_string())
        );
    }

    #[test]
    fn the_current_release_is_not_reported() {
        assert_eq!(newer_than(&serving("v0.1.0"), "0.1.0").unwrap(), None);
        assert_eq!(newer_than(&serving("0.1.0"), "0.1.0").unwrap(), None);
    }

    /// Somebody running a build newer than anything published — during
    /// development, most of the time.
    #[test]
    fn an_older_release_is_not_reported() {
        assert_eq!(newer_than(&serving("v0.1.0"), "0.2.0").unwrap(), None);
    }

    #[test]
    fn no_network_is_not_an_answer_worth_showing() {
        assert!(newer_than(&FakeGitHub(Err(())), "0.1.0").is_err());
    }

    #[test]
    fn an_answer_that_is_not_a_release_is_refused() {
        assert!(newer_than(&FakeGitHub(Ok("<html>rate limited".into())), "0.1.0").is_err());
        assert!(newer_than(&FakeGitHub(Ok("{}".into())), "0.1.0").is_err());
    }
}
