//! Streaming one response body into a sink, for media a channel fetches.
//!
//! [`super::send`] buffers a whole response under one deadline, which suits
//! API calls. A download instead hands each read to a [`DownloadSink`] as it
//! arrives, stops at a byte cap, and bounds every wait for the server on its
//! own, so a large file on a slow link is not cut off while it still moves.

use alloc::string::String;
use core::future::Future;

use barracuda_bulk_memory::BulkBox;
use embassy_time::Duration;
use reqwless::request::RequestBuilder as _;

use super::{
    Deadline, Error, Method, PacedBody, RequestBody, HEADER_BUFFER_SIZE, READ_BUFFER_SIZE,
    SEND_TIMEOUT,
};

/// Default [`DownloadLimits::idle_timeout`]: the longest wait for the server,
/// matching [`SEND_TIMEOUT`].
pub const DOWNLOAD_IDLE_TIMEOUT: Duration = SEND_TIMEOUT;

/// One request whose response body [`download`] streams, built the way
/// [`super::GatewayHttp::send`] takes its arguments.
#[derive(Clone, Copy, Debug)]
pub struct DownloadRequest<'a, B = ()> {
    /// Request method.
    pub method: Method,
    /// Absolute `http://` or `https://` URL.
    pub url: &'a str,
    /// Extra request headers.
    pub headers: &'a [(&'a str, &'a str)],
    /// Request body; `()` sends none.
    pub body: B,
}

impl<'a> DownloadRequest<'a> {
    /// A `GET` of `url` without extra headers.
    #[must_use]
    pub const fn get(url: &'a str) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: &[],
            body: (),
        }
    }
}

impl<'a, B> DownloadRequest<'a, B> {
    /// The same request with `headers`.
    #[must_use]
    pub fn with_headers(self, headers: &'a [(&'a str, &'a str)]) -> Self {
        Self { headers, ..self }
    }
}

/// Limits of one [`download`].
#[derive(Clone, Copy, Debug)]
pub struct DownloadLimits {
    /// Largest body accepted; a longer one fails with
    /// [`Error::BodyTooLarge`], as does a `Content-Length` above it before
    /// any of the body is read.
    pub max_bytes: u64,
    /// Longest wait for the server: for connecting through the response
    /// head, for each write of the request body, and for each read of the
    /// response body. Time the sink takes does not count.
    pub idle_timeout: Duration,
}

/// Status line and headers of a downloaded response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadHead {
    /// HTTP status code.
    pub status: u16,
    /// The `Content-Type` header, as sent.
    pub content_type: Option<String>,
    /// The `Content-Length` header, when the body is not chunked.
    pub content_length: Option<u64>,
}

/// Where [`download`] puts a response.
pub trait DownloadSink {
    /// Why the sink stopped the download.
    type Error;

    /// Takes the response head before any of its body; an error stops the
    /// download, as for a status the sink does not want.
    fn head(&mut self, head: &DownloadHead) -> impl Future<Output = Result<(), Self::Error>>;

    /// Takes the next bytes of the body.
    fn write(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Failure of one [`download`].
#[derive(Debug, thiserror::Error)]
pub enum DownloadError<E> {
    /// The exchange failed, idled past its limit, or the body was too large.
    #[error(transparent)]
    Http(Error),
    /// The sink stopped the download.
    #[error("download sink failed")]
    Sink(E),
}

/// Sends `request` on a fresh connection and streams the response body into
/// `sink`, returning the response head once the body has ended.
///
/// The body is given to the sink whatever the status; the sink sees the
/// status first in [`DownloadSink::head`].
///
/// # Errors
///
/// Returns [`DownloadError::Http`] with [`Error::Timeout`] when the server
/// is silent for [`DownloadLimits::idle_timeout`], with
/// [`Error::BodyTooLarge`] past [`DownloadLimits::max_bytes`], or with the
/// transport's failure; and [`DownloadError::Sink`] when the sink fails.
///
/// # Cancel safety
///
/// Dropping the future drops the connection; whatever the sink received
/// stays with the sink.
pub async fn download<T, D, B, S>(
    http_clients: &http_client::ClientFactory<'_, T, D>,
    request: DownloadRequest<'_, B>,
    limits: DownloadLimits,
    sink: &mut S,
) -> Result<DownloadHead, DownloadError<S::Error>>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
    B: RequestBody,
    S: DownloadSink,
{
    // The clock pauses whenever the download waits on the request body or
    // the sink, and restarts from the full limit at every exchange with the
    // server.
    let deadline = Deadline::new(limits.idle_timeout, true);
    let exchange = async {
        let (mut client, tls_configured) = http_clients.create();
        if request.url.starts_with("https://") && !tls_configured {
            return Err(DownloadError::Http(Error::TlsNotConfigured));
        }
        let body = PacedBody {
            body: request.body,
            deadline: &deadline,
        };
        let mut header_buffer = BulkBox::<[u8]>::new_zeroed_slice(HEADER_BUFFER_SIZE);
        let http = |error: reqwless::Error| DownloadError::Http(Error::Reqwless(error));
        let mut sent = client
            .request(request.method, request.url)
            .await
            .map_err(http)?
            .headers(request.headers)
            .body(body);
        let response = sent.send(&mut header_buffer[..]).await.map_err(http)?;
        let head = DownloadHead {
            status: response.status.0,
            content_type: response
                .headers()
                .find(|(name, _value)| name.eq_ignore_ascii_case("content-type"))
                .and_then(|(_name, value)| core::str::from_utf8(value).ok())
                .map(|value| String::from(value.trim())),
            content_length: response
                .content_length
                .and_then(|length| u64::try_from(length).ok()),
        };
        if head
            .content_length
            .is_some_and(|length| length > limits.max_bytes)
        {
            return Err(DownloadError::Http(Error::BodyTooLarge));
        }
        deadline.pause();
        sink.head(&head).await.map_err(DownloadError::Sink)?;
        let mut reader = response.body().reader();
        let mut read_buffer = BulkBox::<[u8]>::new_zeroed_slice(READ_BUFFER_SIZE);
        let mut received = 0_u64;
        loop {
            deadline.resume();
            let read = embedded_io_async::Read::read(&mut reader, &mut read_buffer)
                .await
                .map_err(|error| {
                    http(reqwless::Error::Network(embedded_io::Error::kind(&error)))
                })?;
            deadline.pause();
            if read == 0 {
                return Ok(head);
            }
            let chunk = read_buffer
                .get(..read)
                .ok_or(DownloadError::Http(Error::Reqwless(reqwless::Error::Codec)))?;
            received = u64::try_from(read)
                .ok()
                .and_then(|read| received.checked_add(read))
                .filter(|received| *received <= limits.max_bytes)
                .ok_or(DownloadError::Http(Error::BodyTooLarge))?;
            sink.write(chunk).await.map_err(DownloadError::Sink)?;
        }
    };
    futures_lite::future::or(exchange, async {
        deadline.expired().await;
        Err(DownloadError::Http(Error::Timeout))
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

    use alloc::{boxed::Box, format, vec::Vec};

    use barracuda_platform_test::{ScriptStep, ScriptedStack};
    use embassy_time::{Instant, Timer};
    use futures_lite::future::block_on;
    use http_client::ClientFactory;

    use super::*;

    const LIMITS: DownloadLimits = DownloadLimits {
        max_bytes: 16,
        idle_timeout: Duration::from_millis(100),
    };

    #[derive(Default)]
    struct Collect {
        head: Option<DownloadHead>,
        body: Vec<u8>,
        writes: usize,
        /// Time each write takes, which must not count against the limit.
        delay: Option<Duration>,
        refuse_status: Option<u16>,
    }

    impl DownloadSink for Collect {
        type Error = u16;

        async fn head(&mut self, head: &DownloadHead) -> Result<(), u16> {
            self.head = Some(head.clone());
            match self.refuse_status {
                Some(status) if status == head.status => Err(status),
                _ => Ok(()),
            }
        }

        async fn write(&mut self, bytes: &[u8]) -> Result<(), u16> {
            if let Some(delay) = self.delay {
                Timer::after(delay).await;
            }
            self.writes += 1;
            self.body.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn run(
        steps: impl IntoIterator<Item = ScriptStep>,
        request: DownloadRequest<'_>,
        limits: DownloadLimits,
        sink: &mut Collect,
    ) -> (
        Result<DownloadHead, DownloadError<u16>>,
        &'static ScriptedStack,
    ) {
        let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(steps)));
        let factory = ClientFactory::from_network(network, network);
        (block_on(download(&factory, request, limits, sink)), network)
    }

    fn chunked(content_type: &str, chunks: &[&str]) -> ScriptStep {
        let mut bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\n\r\n"
        );
        for chunk in chunks {
            bytes.push_str(&format!("{:x}\r\n{chunk}\r\n", chunk.len()));
        }
        bytes.push_str("0\r\n\r\n");
        ScriptStep::Response {
            bytes: bytes.into_bytes(),
            max_read: 5,
            pending_after: false,
        }
    }

    #[test]
    fn a_body_streams_into_the_sink_with_its_head() {
        let mut sink = Collect::default();
        let headers = [("Authorization", "Bearer t")];
        let (result, network) = run(
            [ScriptStep::response(200, "image/png", b"0123456789", 3)],
            DownloadRequest::get("http://cdn.test/a.png").with_headers(&headers),
            LIMITS,
            &mut sink,
        );

        let head = result.expect("download");
        assert_eq!(head.status, 200);
        assert_eq!(head.content_type.as_deref(), Some("image/png"));
        assert_eq!(head.content_length, Some(10));
        assert_eq!(sink.head, Some(head));
        assert_eq!(sink.body, b"0123456789".to_vec());
        let requests = network.requests();
        assert!(requests[0].starts_with("GET /a.png HTTP/1.1\r\n"));
        assert!(requests[0].contains("Authorization: Bearer t\r\n"));
    }

    #[test]
    fn a_declared_length_over_the_cap_fails_before_the_body() {
        let mut sink = Collect::default();
        let (result, _network) = run(
            [ScriptStep::response(200, "video/mp4", &[7; 17], usize::MAX)],
            DownloadRequest::get("http://cdn.test/v.mp4"),
            LIMITS,
            &mut sink,
        );

        assert!(matches!(
            result,
            Err(DownloadError::Http(Error::BodyTooLarge))
        ));
        assert_eq!(sink.head, None);
        assert!(sink.body.is_empty());
    }

    #[test]
    fn a_chunked_body_over_the_cap_stops_at_the_cap() {
        let mut sink = Collect::default();
        let (result, _network) = run(
            [chunked("audio/ogg", &["0123456789", "abcdefghij"])],
            DownloadRequest::get("http://cdn.test/v.ogg"),
            LIMITS,
            &mut sink,
        );

        assert!(matches!(
            result,
            Err(DownloadError::Http(Error::BodyTooLarge))
        ));
        assert_eq!(sink.head.and_then(|head| head.content_length), None);
        assert!(sink.body.len() <= 16);
    }

    #[test]
    fn the_sink_can_refuse_a_status() {
        let mut sink = Collect {
            refuse_status: Some(404),
            ..Collect::default()
        };
        let (result, _network) = run(
            [ScriptStep::json(404, r#"{"error":"gone"}"#)],
            DownloadRequest::get("http://cdn.test/gone"),
            LIMITS,
            &mut sink,
        );

        assert!(matches!(result, Err(DownloadError::Sink(404))));
        assert!(sink.body.is_empty());
    }

    #[test]
    fn a_silent_server_times_out_after_the_idle_limit() {
        let mut sink = Collect::default();
        let started = Instant::now();
        let (result, _network) = run(
            [ScriptStep::pending_after_headers(200, "image/jpeg")],
            DownloadRequest::get("http://cdn.test/slow.jpg"),
            LIMITS,
            &mut sink,
        );

        assert!(matches!(result, Err(DownloadError::Http(Error::Timeout))));
        assert!(started.elapsed() >= LIMITS.idle_timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            sink.head.and_then(|head| head.content_type).as_deref(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn a_slow_sink_does_not_count_against_the_idle_limit() {
        let mut sink = Collect {
            delay: Some(Duration::from_millis(60)),
            ..Collect::default()
        };
        let started = Instant::now();
        let (result, _network) = run(
            [ScriptStep::response(200, "text/plain", b"0123456789", 2)],
            DownloadRequest::get("http://cdn.test/a.txt"),
            LIMITS,
            &mut sink,
        );

        assert!(result.is_ok());
        assert!(sink.writes >= 2);
        assert!(started.elapsed() > LIMITS.idle_timeout);
        assert_eq!(sink.body, b"0123456789".to_vec());
    }

    #[test]
    fn https_without_tls_is_refused() {
        let mut sink = Collect::default();
        let (result, network) = run(
            [],
            DownloadRequest::get("https://cdn.test/a.png"),
            LIMITS,
            &mut sink,
        );

        assert!(matches!(
            result,
            Err(DownloadError::Http(Error::TlsNotConfigured))
        ));
        assert_eq!(network.connect_count(), 0);
    }
}
