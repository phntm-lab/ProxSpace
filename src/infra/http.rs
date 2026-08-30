//! The real HTTP client, over ureq.
//!
//! The only implementation of [`HttpClient`] that reaches the network; the
//! tests bring their own.

use std::time::Duration;

use crate::ports::http::{HttpClient, HttpError, Request, Response};

/// Budget for everything up to and including the response headers: name
/// resolution, connecting, TLS, and the server's first answer. Short on
/// purpose, so an unreachable or wedged mirror fails in seconds. How it
/// reaches the headers without also limiting the body is explained where it
/// is applied, in [`UreqClient::new`].
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The real client, on `ureq`.
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
