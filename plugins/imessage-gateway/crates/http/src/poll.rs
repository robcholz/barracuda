//! Polling one origin over a single kept-alive connection.
//!
//! A channel that has no push connection polls its service: [`poll`] opens
//! one connection to an origin and sends the poller's `GET` and `POST`
//! requests over it until the connection ends or the poller stops. The poller
//! decides what to ask for and when ([`Poller::next`]), and handles each
//! buffered response ([`Poller::response`]); the reqwless types stay inside
//! this crate.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;

use embassy_time::{with_timeout, Duration};
use reqwless::request::RequestBuilder as _;

use crate::Error;

/// Limits of a [`poll`] connection.
#[derive(Clone, Copy, Debug)]
pub struct PollLimits {
    /// Limit for resolving, connecting, and the TLS handshake.
    pub connect_timeout: Duration,
    /// Limit for one request and its whole response, unless the request
    /// names its own ([`PollRequest::timeout`]).
    pub request_timeout: Duration,
    /// Largest body buffered; the rest is read and dropped, and the response
    /// is marked [`Polled::truncated`].
    pub max_body: usize,
    /// Bytes kept from the end of a truncated body in [`Polled::tail`]; zero
    /// keeps none. Nothing is allocated for it unless a body is truncated.
    pub max_tail: usize,
    /// Bytes for a response's status line and headers.
    pub max_head: usize,
}

/// One request of a [`poll`] connection.
#[derive(Debug)]
pub struct PollRequest {
    /// Request target: path and query, below the origin.
    pub target: String,
    /// The body of a `POST`; `None` sends a `GET`.
    pub body: Option<Vec<u8>>,
    /// Limit for this request and its whole response, in place of
    /// [`PollLimits::request_timeout`].
    pub timeout: Option<Duration>,
}

impl PollRequest {
    /// A `POST` of `body` to `target`.
    #[must_use]
    pub fn post(target: String, body: Vec<u8>) -> Self {
        Self {
            target,
            body: Some(body),
            timeout: None,
        }
    }

    /// The same request with its own time limit.
    #[must_use]
    pub fn within(self, timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }
}

impl From<String> for PollRequest {
    /// A `GET` of `target`.
    fn from(target: String) -> Self {
        Self {
            target,
            body: None,
            timeout: None,
        }
    }
}

/// One buffered response of a poll.
#[derive(Debug)]
pub struct Polled {
    /// HTTP status code.
    pub status: u16,
    /// `Retry-After` in seconds, when the server sent it as a number.
    pub retry_after: Option<u64>,
    /// The body, or its first [`PollLimits::max_body`] bytes when truncated.
    pub body: Vec<u8>,
    /// Whether the body was longer than [`PollLimits::max_body`].
    pub truncated: bool,
    /// When truncated, the body's last [`PollLimits::max_tail`] bytes (which
    /// may overlap [`Self::body`]); empty otherwise.
    pub tail: Vec<u8>,
}

/// What a [`poll`] connection asks for and how it handles the answers.
pub trait Poller {
    /// Why the poller stopped the connection.
    type Error;

    /// Waits until the next poll is due and returns its request, or an error
    /// to stop.
    fn next(&mut self) -> impl Future<Output = Result<PollRequest, Self::Error>>;

    /// Adds the headers of the request [`Self::next`] returned last.
    fn headers<'s>(&'s self, headers: &mut Vec<(&'s str, &'s str)>);

    /// Handles one response; an error stops the connection.
    fn response(&mut self, response: Polled) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Why a [`poll`] connection ended.
#[derive(Debug)]
pub enum PollEnd<E> {
    /// The connection could not be opened.
    Connect(Error),
    /// A request failed after `answered` responses on this connection; the
    /// server may simply have closed a kept-alive connection.
    Request {
        /// The failure.
        error: Error,
        /// Responses handled on this connection before it failed.
        answered: usize,
    },
    /// The poller stopped.
    Poller(E),
}

/// Opens one connection to `origin` and polls over it until a request fails
/// or the poller stops.
///
/// # Cancel safety
///
/// Dropping the future drops the connection; the next call opens a new one.
pub async fn poll<T, D, P>(
    http_clients: &http_client::ClientFactory<'_, T, D>,
    origin: &str,
    limits: PollLimits,
    poller: &mut P,
) -> PollEnd<P::Error>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    P: Poller,
{
    let (mut client, tls_configured) = http_clients.create();
    if origin.starts_with("https://") && !tls_configured {
        return PollEnd::Connect(Error::TlsNotConfigured);
    }
    let mut resource = match with_timeout(limits.connect_timeout, client.resource(origin)).await {
        Ok(Ok(resource)) => resource,
        Ok(Err(error)) => return PollEnd::Connect(Error::Reqwless(error)),
        Err(_timeout) => return PollEnd::Connect(Error::Timeout),
    };
    let mut head = alloc::vec![0_u8; limits.max_head];
    let mut answered = 0_usize;
    loop {
        let request = match poller.next().await {
            Ok(request) => request,
            Err(error) => return PollEnd::Poller(error),
        };
        let mut headers = Vec::new();
        poller.headers(&mut headers);
        let exchange = async {
            let response = match &request.body {
                Some(body) => {
                    resource
                        .post(&request.target)
                        .headers(&headers)
                        .body(body.as_slice())
                        .send(&mut head)
                        .await?
                }
                None => {
                    resource
                        .get(&request.target)
                        .headers(&headers)
                        .send(&mut head)
                        .await?
                }
            };
            let status = response.status.0;
            let retry_after = response
                .headers()
                .find(|(name, _value)| name.eq_ignore_ascii_case("retry-after"))
                .and_then(|(_name, value)| core::str::from_utf8(value).ok()?.trim().parse().ok());
            let declared = response.content_length.unwrap_or(0);
            let mut reader = response.body().reader();
            let mut body = Vec::with_capacity(declared.min(limits.max_body));
            let mut tail = VecDeque::new();
            let mut truncated = false;
            let mut chunk = [0_u8; 512];
            loop {
                let read = embedded_io_async::Read::read(&mut reader, &mut chunk)
                    .await
                    .map_err(|error| reqwless::Error::Network(embedded_io::Error::kind(&error)))?;
                if read == 0 {
                    break;
                }
                let bytes = chunk.get(..read).unwrap_or_default();
                let room = limits.max_body.saturating_sub(body.len());
                let (kept, rest) = bytes.split_at(room.min(bytes.len()));
                body.extend_from_slice(kept);
                if rest.is_empty() {
                    continue;
                }
                if !truncated {
                    truncated = true;
                    // The tail starts with the end of the buffered prefix, so
                    // it is the body's last bytes even when the body is only
                    // a little over the limit.
                    let seed = body.len().saturating_sub(limits.max_tail);
                    tail.extend(body.get(seed..).unwrap_or_default());
                }
                if limits.max_tail > 0 {
                    tail.extend(rest);
                    let excess = tail.len().saturating_sub(limits.max_tail);
                    tail.drain(..excess);
                }
            }
            Ok::<_, reqwless::Error>(Polled {
                status,
                retry_after,
                body,
                truncated,
                tail: tail.into(),
            })
        };
        let timeout = request.timeout.unwrap_or(limits.request_timeout);
        let polled = match with_timeout(timeout, exchange).await {
            Ok(Ok(polled)) => polled,
            Ok(Err(error)) => {
                return PollEnd::Request {
                    error: Error::Reqwless(error),
                    answered,
                }
            }
            Err(_timeout) => {
                return PollEnd::Request {
                    error: Error::Timeout,
                    answered,
                }
            }
        };
        drop(headers);
        drop(request);
        answered = answered.saturating_add(1);
        if let Err(error) = poller.response(polled).await {
            return PollEnd::Poller(error);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    extern crate std;

    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use std::boxed::Box;

    use barracuda_platform_test::{ScriptStep, ScriptedStack};
    use futures_lite::future::block_on;

    use super::*;

    const LIMITS: PollLimits = PollLimits {
        connect_timeout: Duration::from_secs(5),
        request_timeout: Duration::from_secs(5),
        max_body: 8,
        max_tail: 0,
        max_head: 1024,
    };

    /// Status, `Retry-After`, body, truncated, and tail of one response.
    type Seen = (u16, Option<u64>, Vec<u8>, bool, Vec<u8>);

    /// What [`Recorder`] asks for: `GET /p/<n>` unless a request is queued.
    struct Recorder {
        asked: usize,
        stop_after: usize,
        queued: Vec<PollRequest>,
        seen: Vec<Seen>,
    }

    impl Recorder {
        fn new(stop_after: usize) -> Self {
            Self {
                asked: 0,
                stop_after,
                queued: Vec::new(),
                seen: Vec::new(),
            }
        }
    }

    impl Poller for Recorder {
        type Error = &'static str;

        async fn next(&mut self) -> Result<PollRequest, &'static str> {
            if self.seen.len() >= self.stop_after {
                return Err("done");
            }
            self.asked += 1;
            if !self.queued.is_empty() {
                return Ok(self.queued.remove(0));
            }
            Ok(format!("/p/{}", self.asked).into())
        }

        fn headers<'s>(&'s self, headers: &mut Vec<(&'s str, &'s str)>) {
            headers.push(("X-Key", "k"));
        }

        async fn response(&mut self, response: Polled) -> Result<(), &'static str> {
            self.seen.push((
                response.status,
                response.retry_after,
                response.body,
                response.truncated,
                response.tail,
            ));
            Ok(())
        }
    }

    fn raw(status: u16, headers: &str, body: &str) -> ScriptStep {
        ScriptStep::Response {
            bytes: format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{headers}Connection: keep-alive\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
            max_read: 3,
            pending_after: false,
        }
    }

    fn run_with(
        steps: Vec<ScriptStep>,
        limits: PollLimits,
        mut recorder: Recorder,
    ) -> (PollEnd<&'static str>, Recorder, &'static ScriptedStack) {
        let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
        let factory = http_client::ClientFactory::from_network(network, network);
        let end = block_on(poll(&factory, "http://svc.test", limits, &mut recorder));
        (end, recorder, network)
    }

    fn run(
        steps: Vec<ScriptStep>,
        stop_after: usize,
    ) -> (PollEnd<&'static str>, Recorder, &'static ScriptedStack) {
        run_with(steps, LIMITS, Recorder::new(stop_after))
    }

    #[test]
    fn polls_reuse_one_connection_and_report_status_retry_after_and_truncation() {
        let (end, recorder, network) = run(
            vec![
                raw(200, "", "[1]"),
                raw(429, "Retry-After: 7\r\n", "{}"),
                raw(200, "", "0123456789"),
            ],
            3,
        );
        assert!(matches!(end, PollEnd::Poller("done")));
        assert_eq!(network.connect_count(), 1);
        assert_eq!(
            recorder.seen,
            vec![
                (200, None, b"[1]".to_vec(), false, Vec::new()),
                (429, Some(7), b"{}".to_vec(), false, Vec::new()),
                (200, None, b"01234567".to_vec(), true, Vec::new()),
            ]
        );
        let requests = network.requests();
        assert!(requests[0].starts_with("GET /p/1 HTTP/1.1\r\n"));
        assert!(requests[0].contains("X-Key: k\r\n"));
        assert!(requests[2].starts_with("GET /p/3 "));
    }

    #[test]
    fn a_truncated_body_keeps_its_last_bytes_in_the_tail() {
        let limits = PollLimits {
            max_tail: 4,
            ..LIMITS
        };
        let (_end, recorder, _network) = run_with(
            vec![
                raw(200, "", "0123456789abcdef"),
                raw(200, "", "012345678"),
                raw(200, "", "01234567"),
            ],
            limits,
            Recorder::new(3),
        );
        assert_eq!(
            recorder.seen,
            vec![
                (200, None, b"01234567".to_vec(), true, b"cdef".to_vec()),
                // Only one byte past the limit: the tail overlaps the body.
                (200, None, b"01234567".to_vec(), true, b"5678".to_vec()),
                (200, None, b"01234567".to_vec(), false, Vec::new()),
            ]
        );
    }

    #[test]
    fn a_request_can_post_a_body_within_its_own_time_limit() {
        let mut recorder = Recorder::new(2);
        recorder.queued = vec![
            PollRequest::post("/q".to_string(), b"{\"a\":1}".to_vec()),
            PollRequest::from("/slow".to_string()).within(Duration::from_millis(50)),
        ];
        let (end, recorder, network) = run_with(
            vec![
                raw(200, "", "ok"),
                ScriptStep::pending_after_headers(200, "application/json"),
            ],
            PollLimits {
                request_timeout: Duration::from_secs(60),
                ..LIMITS
            },
            recorder,
        );
        let PollEnd::Request { error, answered } = end else {
            panic!("expected the slow request to time out");
        };
        assert!(matches!(error, Error::Timeout));
        assert_eq!(answered, 1);
        assert_eq!(recorder.seen.len(), 1);
        let requests = network.requests();
        assert!(requests[0].starts_with("POST /q HTTP/1.1\r\n"));
        assert!(requests[0].contains("X-Key: k\r\n"));
        assert!(requests[0].ends_with("{\"a\":1}"));
        assert!(requests[1].starts_with("GET /slow "));
    }

    #[test]
    fn a_failed_request_reports_how_many_were_answered() {
        let (end, _recorder, _network) = run(
            vec![
                raw(200, "", "[]"),
                ScriptStep::ConnectError(embedded_io::ErrorKind::ConnectionReset),
            ],
            9,
        );
        let PollEnd::Request { answered, .. } = end else {
            panic!("expected a request failure");
        };
        assert_eq!(answered, 1);
        let (end, _recorder, _network) = run(vec![], 9);
        assert!(matches!(end, PollEnd::Connect(_)));
        let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([])));
        let factory = http_client::ClientFactory::from_network(network, network);
        let mut recorder = Recorder::new(1);
        let end = block_on(poll(&factory, "https://svc.test", LIMITS, &mut recorder));
        assert!(matches!(end, PollEnd::Connect(Error::TlsNotConfigured)));
        assert_eq!(recorder.asked.to_string(), "0");
    }
}
