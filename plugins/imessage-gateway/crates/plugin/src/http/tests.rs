#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use alloc::{boxed::Box, string::ToString, vec::Vec};

use barracuda_platform_test::{ScriptStep, ScriptedStack};
use embassy_time::{Duration, Instant, Timer};
use futures_lite::{future::block_on, stream};
use gateway::{BinaryBody, BinaryChunk};
use http_client::ClientFactory;

use super::{send_limited, send_within, Error, Method, Multipart};

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
