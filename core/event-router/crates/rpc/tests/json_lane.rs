#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_rpc::{
    json_schema, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcAddress, RpcError,
    RpcLaneStorage, RpcRegistry,
};
use futures_lite::future::block_on;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct EchoRequest<'a> {
    city: &'a str,
    days: u8,
}

struct Echo;

impl JsonRpcSchema for Echo {
    const ADDRESS: &'static str = "tool.echo";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("echo", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("echo", response);
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 64;
}

struct Object;

impl JsonRpcSchema for Object {
    const ADDRESS: &'static str = "tool.object";
    const REQUEST_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const RESPONSE_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 4;
}

struct Validate;

impl JsonRpcSchema for Validate {
    const ADDRESS: &'static str = "tool.validate";
    const REQUEST_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const RESPONSE_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const MAX_REQUEST_BYTES: usize = 16;
    const MAX_RESPONSE_BYTES: usize = 3;
}

fn registry() -> RpcRegistry<1, 128, 1> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
    RpcRegistry::new(lanes)
}

#[test]
fn raw_json_is_read_and_written_in_the_rpc_lane() {
    let registry = registry();
    registry
        .register_json::<Echo, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let raw = request.as_str()?;
                assert_eq!(raw, r#"{"city":"Raleigh","days":3}"#);
                let typed: EchoRequest<'_> = request.deserialize()?;
                assert_eq!(typed.city, "Raleigh");
                assert_eq!(typed.days, 3);
                response.write(raw).await
            },
        )
        .expect("register JSON endpoint");

    let address = RpcAddress::try_from("tool.echo").expect("valid address");
    let client = registry.client();
    let call = client
        .call_json(&address, r#"{"city":"Raleigh","days":3}"#)
        .expect("start JSON call");
    let response = block_on(call).expect("call JSON endpoint");

    assert_eq!(
        response.as_str().expect("valid lane-backed JSON"),
        r#"{"city":"Raleigh","days":3}"#
    );
    assert_eq!(
        registry
            .rpcs_by_visibility("agent")
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>(),
        vec!["tool.echo"]
    );

    let info = client
        .json_method_info(&address)
        .expect("read JSON method metadata");
    assert_eq!(info.address().as_ref(), Echo::ADDRESS);
    assert_eq!(info.request_schema(), Echo::REQUEST_SCHEMA);
    assert_eq!(info.response_schema(), Echo::RESPONSE_SCHEMA);
    assert_eq!(info.max_request_bytes(), Echo::MAX_REQUEST_BYTES);
    assert_eq!(info.max_response_bytes(), Echo::MAX_RESPONSE_BYTES);
    assert!(Echo::REQUEST_SCHEMA.as_str().contains("city"));
    assert!(Echo::RESPONSE_SCHEMA.as_str().contains("days"));
}

#[test]
fn a_json_value_is_serialized_directly_into_the_lane() {
    let registry = registry();
    registry
        .register_json::<Object, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                assert_eq!(request.as_str()?, r#"{"city":"Raleigh","days":3}"#);
                response.write("true").await
            },
        )
        .expect("register JSON endpoint");

    let address = RpcAddress::try_from("tool.object").expect("valid address");
    let request = json!({ "city": "Raleigh", "days": 3 });
    let client = registry.client();
    let response = block_on(
        client
            .call_json(&address, &request)
            .expect("start JSON object call"),
    )
    .expect("call JSON object endpoint");

    assert_eq!(response.as_str().expect("valid response"), "true");
}

#[test]
fn json_calls_reject_invalid_or_oversized_documents() {
    let registry = registry();
    registry
        .register_json::<Validate, _>(
            "system",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("null").await
            },
        )
        .expect("register JSON endpoint");
    let address = RpcAddress::try_from("tool.validate").expect("valid address");
    let client = registry.client();

    assert!(client.call_json(&address, "{").is_err());
    assert!(matches!(
        client.call_json(&address, r#"{"value":1234567}"#),
        Err(RpcError::FrameTooLarge {
            size: 17,
            capacity: 16,
        })
    ));

    let response = block_on(
        client
            .call_json(&address, "null")
            .expect("start bounded response call"),
    );
    assert!(matches!(
        response,
        Err(RpcError::FrameTooLarge {
            size: 4,
            capacity: 3,
        })
    ));
}
