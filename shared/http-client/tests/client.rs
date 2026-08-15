#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::rc::Rc;

use barracuda_net::testing::{ScriptStep, ScriptedStack};
use futures_lite::{future::block_on, stream};
use http_client::{Body, BodyError, HttpClient, Method, Request, ReqwlessClient, Response};

#[test]
fn sends_a_json_request_and_reads_the_complete_response() {
    block_on(async {
        let network = ScriptedStack::new([ScriptStep::json(200, r#"{"ok":true}"#)]);
        let client = ReqwlessClient::new(&network, 2048, 512);

        let response = client
            .execute(
                Request::post("http://example.test/v1/messages")
                    .header("Authorization", "Bearer secret")
                    .json(r#"{"text":"hello"}"#),
            )
            .await;

        assert!(matches!(
            response,
            Ok(Response { status: 200, body }) if body == br#"{"ok":true}"#
        ));
        let requests = network.requests();
        let request = requests.first().map(String::as_str).unwrap_or_default();
        assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer secret\r\n"));
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.ends_with(r#"{"text":"hello"}"#));
    });
}

#[test]
fn streams_an_unknown_length_request_body_with_chunked_encoding() {
    block_on(async {
        let network = ScriptedStack::new([ScriptStep::json(201, r#"{"id":"file-1"}"#)]);
        let client = ReqwlessClient::new(&network, 2048, 512);
        let chunks = stream::iter([Ok(b"first".to_vec()), Ok(b"second".to_vec())]);

        let response = client
            .execute(
                Request::new(Method::Post, "http://example.test/upload")
                    .content_type("application/octet-stream")
                    .body(Body::stream(Box::pin(chunks))),
            )
            .await;

        assert!(matches!(response, Ok(Response { status: 201, .. })));
        let requests = network.requests();
        let request = requests.first().map(String::as_str).unwrap_or_default();
        assert!(request.contains("Transfer-Encoding: chunked\r\n"));
        assert!(request.ends_with("5\r\nfirst\r\n6\r\nsecond\r\n0\r\n\r\n"));
    });
}

#[test]
fn preserves_a_request_stream_failure() {
    block_on(async {
        let network = ScriptedStack::new([ScriptStep::json(200, "{}")]);
        let client = ReqwlessClient::new(&network, 2048, 512);
        let chunks = stream::iter([Err(BodyError::failed("source stopped"))]);

        let result = client
            .execute(
                Request::post("http://example.test/upload").body(Body::stream(Box::pin(chunks))),
            )
            .await;

        assert!(matches!(
            result,
            Err(http_client::Error::Body(BodyError::Failed { message }))
                if message == "source stopped"
        ));
    });
}

#[test]
fn the_client_is_usable_behind_the_object_safe_http_trait() {
    block_on(async {
        let network = ScriptedStack::new([ScriptStep::json(204, "")]);
        let client: Rc<dyn HttpClient> = Rc::new(ReqwlessClient::new(&network, 1024, 128));

        let response = client
            .execute(Request::delete("http://example.test/messages/one"))
            .await;

        assert!(matches!(response, Ok(Response { status: 204, .. })));
    });
}

#[test]
fn rejects_an_invalid_url_before_network_io() {
    block_on(async {
        let network = ScriptedStack::default();
        let client = ReqwlessClient::new(&network, 1024, 128);

        let result = client.execute(Request::get("not-a-url")).await;

        assert!(matches!(result, Err(http_client::Error::InvalidUrl)));
        assert!(network.requests().is_empty());
    });
}
