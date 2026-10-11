#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use alloc::{boxed::Box, rc::Rc, string::ToString, vec::Vec};

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use embassy_time::{Duration, Instant, Timer};
use futures_lite::{future::block_on, stream};
use gateway::{BinaryBody, BinaryChunk, MediaOpenFuture, MediaSource};
use http_client::ClientFactory;

use super::{
    send_limited, send_reading_headers, send_within, Error, Method, Multipart, StreamBody,
};

const TIMEOUT: Duration = Duration::from_millis(100);

fn network(steps: impl IntoIterator<Item = ScriptStep>) -> &'static ScriptedStack {
    Box::leak(Box::new(ScriptedStack::new(steps)))
}

fn factory(
    network: &'static ScriptedStack,
) -> ClientFactory<'static, ScriptedStack, ScriptedStack> {
    ClientFactory::from_network(network, network)
}

/// A multipart body whose stream waits `delay` before each of its chunks.
fn slow_multipart(delay: Duration, chunks: usize) -> super::MultipartBody {
    let data = stream::unfold(0usize, move |sent| async move {
        if sent == chunks {
            return None;
        }
        Timer::after(delay).await;
        Some((
            Ok(BinaryChunk::from(b"PAYLOAD".to_vec())),
            sent.saturating_add(1),
        ))
    });
    let (_content_type, body) = Multipart::new("boundary")
        .file(
            "file",
            "file.bin",
            "application/octet-stream",
            BinaryBody::Stream(Box::pin(data)),
        )
        .finish()
        .expect("multipart body");
    body
}

#[test]
fn a_stalled_response_times_out_as_a_transport_error() {
    let network = network([ScriptStep::pending_after_headers(200, "application/json")]);
    let started = Instant::now();

    let result = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/send",
        &[],
        &b"{}"[..],
    ));

    assert!(matches!(result, Err(Error::Timeout)));
    assert_eq!(
        result.err().map(|error| error.to_string()).as_deref(),
        Some("HTTP request timed out")
    );
    assert!(started.elapsed() >= TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_prompt_response_is_returned_within_the_deadline() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);

    let response = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::GET,
        "http://gateway.test/ok",
        &[],
        (),
    ))
    .expect("response");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
}

#[test]
fn waiting_on_a_streamed_body_does_not_count_against_the_deadline() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
    let started = Instant::now();

    let response = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        slow_multipart(Duration::from_millis(60), 4),
    ))
    .expect("slow caller-fed upload completes");

    assert!(started.elapsed() > TIMEOUT.checked_mul(2).expect("duration"));
    assert_eq!(response.status, 200);
    let requests: Vec<_> = network.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].matches("PAYLOAD").count(), 4);
}

#[test]
fn the_response_after_a_streamed_body_is_bounded() {
    let network = network([ScriptStep::pending_after_headers(200, "application/json")]);

    let result = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        slow_multipart(Duration::from_millis(60), 2),
    ));

    assert!(matches!(result, Err(Error::Timeout)));
    assert_eq!(network.requests().len(), 1);
}

#[test]
fn a_limited_send_rejects_a_body_over_its_limit() {
    let network = network([
        ScriptStep::json(200, r#"{"ok":true}"#),
        ScriptStep::json(200, r#"{"ok":false,"padding":"0123456789"}"#),
    ]);
    let factory = factory(network);

    let fits = block_on(send_limited(
        &factory,
        Method::GET,
        "http://gateway.test/small",
        &[],
        (),
        11,
    ))
    .expect("an 11-byte body fits an 11-byte limit");
    let too_large = block_on(send_limited(
        &factory,
        Method::GET,
        "http://gateway.test/large",
        &[],
        (),
        11,
    ));

    assert_eq!(fits.body, br#"{"ok":true}"#.to_vec());
    assert!(matches!(too_large, Err(Error::BodyTooLarge)));
}

/// A source that claims `len` bytes and yields `bytes` in two chunks.
struct Claimed {
    len: u64,
    bytes: Vec<u8>,
}

impl MediaSource for Claimed {
    fn len(&self) -> u64 {
        self.len
    }

    fn open(&self) -> MediaOpenFuture<'_> {
        let (first, second) = self.bytes.split_at(self.bytes.len() / 2);
        let chunks = [
            Ok(BinaryChunk::from(first.to_vec())),
            Ok(BinaryChunk::from(second.to_vec())),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(chunks)) as gateway::BinaryStream) })
    }
}

fn source_multipart(len: u64, bytes: &[u8]) -> super::MultipartBody {
    let (_content_type, body) = Multipart::new("boundary")
        .text("chat_id", "42")
        .file(
            "photo",
            "photo.jpg",
            "image/jpeg",
            BinaryBody::Source(Rc::new(Claimed {
                len,
                bytes: bytes.to_vec(),
            })),
        )
        .finish()
        .expect("multipart body");
    body
}

#[test]
fn a_source_part_keeps_the_body_length_known() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
    let body = source_multipart(6, b"SOURCE");
    let length = super::RequestBody::len(&body).expect("a known length");
    let probe = body.clone();

    let response = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        body,
    ))
    .expect("upload");

    assert_eq!(response.status, 200);
    assert_eq!(probe.failure(), None);
    let request = &network.requests()[0];
    assert!(request.contains(&alloc::format!("Content-Length: {length}\r\n")));
    let (_head, sent) = request.split_once("\r\n\r\n").expect("request body");
    assert_eq!(sent.len(), length);
    assert!(sent.contains("\r\n\r\nSOURCE\r\n--boundary--\r\n"));
}

#[test]
fn a_source_longer_than_its_length_is_a_failure() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
    let body = source_multipart(4, b"SOURCE");
    let probe = body.clone();

    let _result = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        body,
    ));

    assert_eq!(
        probe.failure().as_deref(),
        Some("media source is longer than its length")
    );
}

fn chunks(parts: &[&[u8]]) -> gateway::BinaryStream {
    let chunks: Vec<_> = parts
        .iter()
        .map(|part| Ok(BinaryChunk::from(part.to_vec())))
        .collect();
    Box::pin(stream::iter(chunks))
}

#[test]
fn a_stream_body_is_sent_whole_with_its_content_length() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
    let body = StreamBody::new(chunks(&[br#"{"data":""#, b"QUJD", br#""}"#]), 15);
    let probe = body.clone();

    let response = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        body,
    ))
    .expect("upload");

    assert_eq!(response.status, 200);
    assert_eq!(probe.failure(), None);
    let request = &network.requests()[0];
    assert!(request.contains("Content-Length: 15\r\n"), "{request}");
    assert!(!request.to_ascii_lowercase().contains("transfer-encoding"));
    assert!(request.ends_with(r#"{"data":"QUJD"}"#), "{request}");
}

#[test]
fn a_known_length_body_is_bounded_per_write_not_as_a_whole() {
    let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
    let parts = stream::unfold(0usize, |sent| async move {
        if sent == 4 {
            return None;
        }
        Timer::after(Duration::from_millis(60)).await;
        Some((
            Ok(BinaryChunk::from(b"PAYLOAD".to_vec())),
            sent.saturating_add(1),
        ))
    });
    let body = StreamBody::new(Box::pin(parts), 28);
    let probe = body.clone();
    let started = Instant::now();

    let response = block_on(send_within(
        TIMEOUT,
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        body,
    ))
    .expect("slow upload of known length completes");

    assert!(started.elapsed() > TIMEOUT.checked_mul(2).expect("duration"));
    assert_eq!(response.status, 200);
    assert_eq!(probe.failure(), None);
    let request = &network.requests()[0];
    assert!(request.contains("Content-Length: 28\r\n"), "{request}");
    assert_eq!(request.matches("PAYLOAD").count(), 4);
}

#[test]
fn a_stream_body_of_the_wrong_length_is_a_failure() {
    let cases: [(&[&[u8]], &str); 2] = [
        (&[b"ABC", b"DEF"], "request body is longer than its length"),
        (&[b"AB"], "request body is shorter than its length"),
    ];
    for (parts, failure) in cases {
        let network = network([ScriptStep::json(200, r#"{"ok":true}"#)]);
        let body = StreamBody::new(chunks(parts), 4);
        let probe = body.clone();

        let _result = block_on(send_within(
            TIMEOUT,
            &factory(network),
            Method::POST,
            "http://gateway.test/upload",
            &[],
            body,
        ));

        assert_eq!(probe.failure().as_deref(), Some(failure));
    }
}

#[test]
fn named_response_headers_are_read() {
    let body = b"{}";
    let mut bytes = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Encrypted-Param:  param-1 \r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    let network = network([ScriptStep::Response {
        bytes,
        max_read: usize::MAX,
        pending_after: false,
    }]);

    let (response, headers) = block_on(send_reading_headers(
        &factory(network),
        Method::POST,
        "http://gateway.test/upload",
        &[],
        &b"x"[..],
        &["x-encrypted-param", "x-error-message"],
    ))
    .expect("response");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"{}".to_vec());
    assert_eq!(headers, [Some("param-1".to_string()), None]);
}
