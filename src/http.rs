//! The one way this binary talks to the network.
//!
//! Everything that downloads goes through the [`HttpClient`] trait rather than
//! calling `ureq` directly, for the reason given in `DECISIONS.md` §2.6: the
//! only download that matters is a hundred-megabyte archive, which no test can
//! afford to fetch. With the trait in the way, the interesting part — resume,
//! hash checking, cleanup after a failure — is testable against a fake, and the
//! real implementation stays small enough to be read rather than tested.
//!
//! The trait hands back a reader instead of writing into a sink: the caller
//! needs to see the response before the body arrives (is the length known? did
//! the server accept the resume offset?) and needs to count bytes as they pass
//! for the progress bar and for the running hash.

use std::io::Read;
use std::time::Duration;

use thiserror::Error;

/// Budget for everything up to and including the response headers: name
/// resolution, connecting, TLS, and the server's first answer. Short on
/// purpose, so an unreachable or wedged mirror fails in seconds. How it
/// reaches the headers without also limiting the body is explained where it
/// is applied, in [`UreqClient::new`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("`{url}` is not a valid URL")]
    BadUrl { url: String },
    #[error("cannot reach `{url}`")]
    Transport {
        url: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{url}` answered {status}{}", explain_status(*status))]
    Status { url: String, status: u16 },
}

/// A response whose body has not been read yet.
pub struct Response {
    /// Number of bytes this response will deliver, when the server says so.
    /// For a resumed download that is the size of the remaining part, not of
    /// the whole file.
    pub body_len: Option<u64>,
    /// True when the server honoured a [`Request::range_from`] offset and is
    /// sending the tail of the file (HTTP 206). False means the offset was
    /// ignored and the body starts from byte zero, so anything already on disk
    /// has to be discarded.
    pub resumed: bool,
    pub body: Box<dyn Read + Send>,
}

/// What to fetch, and from which offset.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    /// Byte offset to continue from, for a download that was interrupted.
    /// Servers may ignore it; [`Response::resumed`] reports what happened.
    pub range_from: Option<u64>,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Request {
        Request {
            url: url.into(),
            range_from: None,
        }
    }

    pub fn resume_from(url: impl Into<String>, offset: u64) -> Request {
        Request {
            url: url.into(),
            range_from: Some(offset),
        }
    }
}

pub trait HttpClient {
    /// Send the request and return once the response headers are in. Any status
    /// outside 2xx is an error: a 404 body is not something a caller of this
    /// trait ever wants to write to disk.
    fn send(&self, request: &Request) -> Result<Response, HttpError>;
}

/// The real client, on `ureq` (`DECISIONS.md` Q13).
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> UreqClient {
        let config = ureq::Agent::config_builder()
            .timeout_resolve(Some(HANDSHAKE_TIMEOUT))
            .timeout_connect(Some(HANDSHAKE_TIMEOUT))
            // Bounds the wait for the response headers as well: in `ureq` each
            // phase is also limited by the deadline of the phase before it, so
            // `recv_response` inherits this one. Setting `timeout_recv_response`
            // explicitly would be worse than leaving it off — it would then
            // become the deadline for receiving the *body* too, which for a
            // hundred-megabyte archive means killing every download slower
            // than the timeout.
            .timeout_send_request(Some(HANDSHAKE_TIMEOUT))
            // Deliberately no deadline on the body. The right limit for it
            // would be a byte-rate one, which `ureq` does not offer: an archive
            // this size legitimately takes many minutes on a slow line, so any
            // total large enough not to kill honest downloads is too large to
            // catch a stall. A stalled transfer is escaped with Ctrl+C twice
            // instead, and the partial file on disk is resumed on the next run.
            .http_status_as_error(false)
            .user_agent(concat!("proxspace/", env!("CARGO_PKG_VERSION")))
            .build();
        UreqClient {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        UreqClient::new()
    }
}

impl HttpClient for UreqClient {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        let mut call = self.agent.get(&request.url);
        if let Some(offset) = request.range_from {
            call = call.header("Range", format!("bytes={offset}-"));
        }

        let response = call.call().map_err(|error| classify(&request.url, error))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(HttpError::Status {
                url: request.url.clone(),
                status,
            });
        }

        // 206 is the only answer that actually continues a file. A server that
        // does not do ranges replies 200 with the whole thing, which is still
        // usable — just not as a continuation.
        let resumed = status == 206;
        let body = response.into_body();
        let body_len = body.content_length();
        Ok(Response {
            body_len,
            resumed,
            body: Box::new(body.into_reader()),
        })
    }
}

fn classify(url: &str, error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => HttpError::BadUrl {
            url: url.to_string(),
        },
        // Everything else — DNS, refused connection, TLS, a timeout — is one
        // thing from the caller's point of view: the bytes did not arrive.
        // Keep the original text as the source so the log still says why.
        other => HttpError::Transport {
            url: url.to_string(),
            source: std::io::Error::other(other),
        },
    }
}

/// Turn the handful of statuses a mirror realistically returns into advice.
/// Anything else is left as the bare number rather than guessed at.
fn explain_status(status: u16) -> &'static str {
    match status {
        403 => " (forbidden — a proxy or mirror is refusing the request)",
        404 => " (not found — the archive is no longer on this mirror)",
        416 => " (the resume offset is past the end of the file)",
        429 => " (rate limited — try again in a few minutes)",
        500..=599 => " (the server is having trouble — try again later)",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_request_carries_the_offset() {
        let request = Request::resume_from("https://example.invalid/a.tar.xz", 1024);
        assert_eq!(request.range_from, Some(1024));
        assert_eq!(
            Request::get("https://example.invalid/a.tar.xz").range_from,
            None
        );
    }

    #[test]
    fn common_statuses_are_explained() {
        assert!(explain_status(404).contains("no longer on this mirror"));
        assert!(explain_status(503).contains("try again later"));
        assert_eq!(explain_status(418), "");
    }

    #[test]
    fn a_status_error_reads_as_a_sentence() {
        let error = HttpError::Status {
            url: "https://example.invalid/a.tar.xz".to_string(),
            status: 404,
        };
        assert_eq!(
            error.to_string(),
            "`https://example.invalid/a.tar.xz` answered 404 \
             (not found — the archive is no longer on this mirror)"
        );
    }
}
