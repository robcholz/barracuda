#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_rpc::{
    json_schema, JsonObjectPayload, JsonPayload, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter,
    RpcAddress, RpcContext, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
};
use futures_lite::future::block_on;
use serde::Deserialize;
use serde_json::json;
use std::cell::Cell;
use std::rc::Rc;

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
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 4;
}

struct Validate;

impl JsonRpcSchema for Validate {
    const ADDRESS: &'static str = "tool.validate";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 16;
    const MAX_RESPONSE_BYTES: usize = 3;
}

struct EmptyAck;

impl JsonRpcSchema for EmptyAck {
    const ADDRESS: &'static str = "tool.empty_ack";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
        r#"{"type":"object","properties":{},"additionalProperties":false}"#
    );
    const MAX_REQUEST_BYTES: usize = 16;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct ComplexValue;

impl JsonRpcSchema for ComplexValue {
    const ADDRESS: &'static str = "tool.complex_value";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 128;
    const MAX_RESPONSE_BYTES: usize = 128;
}

struct InvalidResponse;

impl JsonRpcSchema for InvalidResponse {
    const ADDRESS: &'static str = "tool.invalid_response";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 16;
}

struct MissingResponse;

impl JsonRpcSchema for MissingResponse {
    const ADDRESS: &'static str = "tool.missing_response";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct TypedRequest;

impl JsonRpcSchema for TypedRequest {
    const ADDRESS: &'static str = "tool.typed_request";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 32;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct DirectSelf;

impl JsonRpcSchema for DirectSelf {
    const ADDRESS: &'static str = "tool.direct_self";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct DuplicateEcho;

impl JsonRpcSchema for DuplicateEcho {
    const ADDRESS: &'static str = Echo::ADDRESS;
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct InvalidAddress;

impl JsonRpcSchema for InvalidAddress {
    const ADDRESS: &'static str = "missing_group";
    const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

struct Native;

impl RpcMethod for Native {
    const ADDRESS: &'static str = "native.echo";
    type Request = [u8; 1];
    type Response = [u8; 1];
    type Error = [u8; 1];
    type Input = Unary;
    type Output = Unary;
}

struct InconsistentPayload;

impl JsonPayload for InconsistentPayload {
    fn encoded_len(&self) -> Result<usize, RpcError> {
        Ok(2)
    }

    fn write_json(&self, destination: &mut [u8]) -> Result<usize, RpcError> {
        let capacity = destination.len();
        destination
            .get_mut(..2)
            .ok_or(RpcError::FrameTooLarge { size: 2, capacity })?
            .copy_from_slice(b"{}");
        Ok(1)
    }
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
    assert!(info
        .request_schema()
        .validate(r#"{"city":1,"days":3}"#)
        .is_err());
}

#[test]
fn rpc_enforces_request_and_response_schemas_around_the_handler() {
    let registry = registry();
    let calls = Rc::new(Cell::new(0_u8));
    let handler_calls = Rc::clone(&calls);
    registry
        .register_json::<Echo, _>(
            "agent",
            move |_context, _request: JsonRef, response: JsonWriter| {
                handler_calls.set(handler_calls.get().saturating_add(1));
                async move { response.write("{}").await }
            },
        )
        .expect("register schema-enforced endpoint");

    let address = RpcAddress::try_from(Echo::ADDRESS).expect("valid address");
    let client = registry.client();
    let invalid_request = block_on(
        client
            .call_json(&address, r#"{"city":1,"days":3}"#)
            .expect("start invalid request"),
    );
    assert!(matches!(
        invalid_request,
        Err(RpcError::JsonRequestSchema { address, .. }) if address == Echo::ADDRESS
    ));
    assert_eq!(calls.get(), 0);

    let invalid_response = block_on(
        client
            .call_json(&address, r#"{"city":"Raleigh","days":3}"#)
            .expect("start valid request"),
    );
    assert!(matches!(
        invalid_response,
        Err(RpcError::JsonResponseSchema { address, .. }) if address == Echo::ADDRESS
    ));
    assert_eq!(calls.get(), 1);
}

#[test]
fn raw_payload_calls_cannot_bypass_json_request_validation() {
    let registry = registry();
    let calls = Rc::new(Cell::new(0_u8));
    let handler_calls = Rc::clone(&calls);
    registry
        .register_json::<Echo, _>(
            "agent",
            move |_context, _request: JsonRef, response: JsonWriter| {
                handler_calls.set(handler_calls.get().saturating_add(1));
                async move { response.write(r#"{"city":"Raleigh","days":3}"#).await }
            },
        )
        .expect("register schema-enforced endpoint");
    let address = RpcAddress::try_from(Echo::ADDRESS).expect("valid address");

    block_on(async {
        let (mut writer, mut reader) = registry
            .client()
            .call_payload(&address)
            .expect("start raw payload call");
        let payload = br#"{"city":1,"days":3}"#;
        assert_eq!(writer.write(payload).await, Ok(payload.len()));
        assert!(matches!(
            writer.close().await,
            Err(RpcError::JsonRequestSchema { address, .. }) if address == Echo::ADDRESS
        ));
        assert!(matches!(
            reader.read().await,
            Err(RpcError::JsonRequestSchema { address, .. }) if address == Echo::ADDRESS
        ));
    });
    assert_eq!(calls.get(), 0);
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
fn json_object_payload_writes_fields_without_an_intermediate_value() {
    let fields = |writer: &mut barracuda_rpc::JsonObjectWriter<'_>| {
        writer.field("mode", "\"quiet\"")?;
        writer.field("count", "3")
    };
    let payload = JsonObjectPayload::new(&fields);
    assert_eq!(payload.encoded_len(), Ok(26));
    let mut output = [0_u8; 26];
    let written = payload.write_json(&mut output).expect("write JSON object");

    assert_eq!(written, output.len());
    assert_eq!(
        core::str::from_utf8(&output),
        Ok(r#"{"mode":"quiet","count":3}"#)
    );
}

#[test]
fn json_object_payload_escapes_string_fields() {
    let fields = |writer: &mut barracuda_rpc::JsonObjectWriter<'_>| {
        writer.string_field("event", "gateway.\"received")
    };
    let payload = JsonObjectPayload::new(&fields);
    let mut output = [0_u8; 31];
    let written = payload.write_json(&mut output).expect("write JSON object");

    assert_eq!(
        core::str::from_utf8(output.get(..written).expect("written JSON prefix")),
        Ok(r#"{"event":"gateway.\"received"}"#)
    );
}

#[test]
fn complex_json_values_round_trip_every_token_and_escape() {
    let registry = registry();
    registry
        .register_json::<ComplexValue, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let raw = request.as_str()?;
                response.write(raw).await
            },
        )
        .expect("register complex JSON endpoint");

    let request = json!({
        "array": [null, true, false, -12.5],
        "escaped": "quote=\" slash=\\ control=\n unicode=雪"
    });
    let address = RpcAddress::try_from(ComplexValue::ADDRESS).expect("valid address");
    let response = block_on(
        registry
            .client()
            .call_json(&address, &request)
            .expect("start complex JSON call"),
    )
    .expect("complete complex JSON call");

    let raw = response.as_str().expect("valid JSON response");
    let decoded: serde_json::Value = serde_json::from_str(raw).expect("decode response");
    assert_eq!(decoded, request);
    assert!(raw.contains(r#"quote=\" slash=\\ control=\u000a unicode=雪"#));
}

#[test]
fn owned_string_request_and_empty_object_response_are_supported() {
    let registry = registry();
    registry
        .register_json::<EmptyAck, _>(
            "system",
            |_context, request: JsonRef, response: JsonWriter| async move {
                assert_eq!(request.as_str()?, r#"{"enabled":true}"#);
                response.write("{}").await
            },
        )
        .expect("register empty response endpoint");

    let request = String::from(r#"{"enabled":true}"#);
    let address = RpcAddress::try_from(EmptyAck::ADDRESS).expect("valid address");
    let response = block_on(
        registry
            .client()
            .call_json(&address, &request)
            .expect("start owned String call"),
    )
    .expect("complete owned String call");

    assert_eq!(response.as_str(), Ok("{}"));
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

#[test]
fn payload_encoders_report_their_exact_capacity_boundary() {
    let mut one_byte = [0_u8; 1];
    assert_eq!(
        "{}".write_json(&mut one_byte),
        Err(RpcError::FrameTooLarge {
            size: 2,
            capacity: 1,
        })
    );

    let value = json!([true, false]);
    let mut ten_bytes = [0_u8; 10];
    assert_eq!(value.encoded_len(), Ok(12));
    assert_eq!(
        value.write_json(&mut ten_bytes),
        Err(RpcError::FrameTooLarge {
            size: 12,
            capacity: 10,
        })
    );
}

#[test]
fn handlers_propagate_invalid_typed_requests_and_invalid_responses() {
    let registry = registry();
    registry
        .register_json::<TypedRequest, _>(
            "system",
            |_context, request: JsonRef, response: JsonWriter| async move {
                let _: EchoRequest<'_> = request.deserialize()?;
                response.write("{}").await
            },
        )
        .expect("register typed request endpoint");
    registry
        .register_json::<InvalidResponse, _>(
            "system",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("{").await
            },
        )
        .expect("register invalid response endpoint");

    let typed = RpcAddress::try_from(TypedRequest::ADDRESS).expect("valid typed address");
    let invalid_response =
        RpcAddress::try_from(InvalidResponse::ADDRESS).expect("valid response address");
    let client = registry.client();

    assert!(matches!(
        block_on(
            client
                .call_json(&typed, r#"{"city":1}"#)
                .expect("start call")
        ),
        Err(RpcError::InvalidJson)
    ));
    assert!(matches!(
        block_on(
            client
                .call_json(&invalid_response, "{}")
                .expect("start call")
        ),
        Err(RpcError::InvalidJson)
    ));
}

#[test]
fn missing_response_is_not_treated_as_an_empty_object() {
    let registry = registry();
    registry
        .register_json::<MissingResponse, _>(
            "system",
            |_context, _request: JsonRef, _response: JsonWriter| async move { Ok(()) },
        )
        .expect("register missing response endpoint");
    let address = RpcAddress::try_from(MissingResponse::ADDRESS).expect("valid address");

    assert!(matches!(
        block_on(
            registry
                .client()
                .call_json(&address, "{}")
                .expect("start call")
        ),
        Err(RpcError::MissingUnaryFrame)
    ));
}

#[test]
fn json_and_native_contract_queries_reject_the_wrong_endpoint_kind() {
    let registry = registry();
    registry
        .register_json::<EmptyAck, _>(
            "agent",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("{}").await
            },
        )
        .expect("register JSON endpoint");
    registry
        .register::<Native, _>(
            "system",
            |_context, request: RpcFrame<[u8; 1]>| async move { Ok(Ok(*request.view()?)) },
        )
        .expect("register native endpoint");

    let json_address = RpcAddress::try_from(EmptyAck::ADDRESS).expect("valid JSON address");
    let native_address = RpcAddress::try_from(Native::ADDRESS).expect("valid native address");
    let missing_address = RpcAddress::try_from("missing.method").expect("valid missing address");
    let client = registry.client();

    assert!(matches!(
        client.method_info(&json_address),
        Err(RpcError::NotTypedEndpoint(address)) if address == json_address
    ));
    assert!(matches!(
        client.json_method_info(&native_address),
        Err(RpcError::NotJsonEndpoint(address)) if address == native_address
    ));
    assert!(matches!(
        client.json_method_info(&missing_address),
        Err(RpcError::NotFound(address)) if address == missing_address
    ));
    assert!(matches!(
        client.call_json(&native_address, "{}"),
        Err(RpcError::NotJsonEndpoint(address)) if address == native_address
    ));
}

#[test]
fn registration_validates_addresses_and_shares_one_address_namespace() {
    let registry = registry();
    assert!(matches!(
        registry.register_json::<InvalidAddress, _>(
            "agent",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("{}").await
            },
        ),
        Err(RpcError::Address(_))
    ));

    registry
        .register_json::<Echo, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                response.write(request.as_str()?).await
            },
        )
        .expect("register first endpoint");
    let duplicate =
        registry.register_json::<DuplicateEcho, _>(
            "system",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("{}").await
            },
        );
    let address = RpcAddress::try_from(Echo::ADDRESS).expect("valid address");

    assert!(matches!(
        duplicate,
        Err(RpcError::AlreadyRegistered(duplicate_address)) if duplicate_address == address
    ));
}

#[test]
fn prepared_json_call_survives_unregister_and_stale_tokens_are_rejected() {
    let registry = registry();
    let registration = registry
        .register_json::<Echo, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                response.write(request.as_str()?).await
            },
        )
        .expect("register endpoint");
    let address = RpcAddress::try_from(Echo::ADDRESS).expect("valid address");
    let client = registry.client();
    let prepared = client
        .call_json(&address, r#"{"city":"Raleigh","days":3}"#)
        .expect("prepare call");

    registry
        .unregister(&registration)
        .expect("unregister endpoint");
    let response = block_on(prepared).expect("prepared call remains alive");
    assert_eq!(response.as_str(), Ok(r#"{"city":"Raleigh","days":3}"#));
    assert!(matches!(
        client.call_json(&address, "{}"),
        Err(RpcError::NotFound(missing)) if missing == address
    ));
    assert!(matches!(
        registry.unregister(&registration),
        Err(RpcError::StaleRegistration(stale)) if stale == address
    ));
}

#[test]
fn json_direct_self_call_is_rejected_before_lane_acquisition() {
    let registry = registry();
    registry
        .register_json::<DirectSelf, _>(
            "system",
            |context: RpcContext, _request: JsonRef, response: JsonWriter| async move {
                let address = RpcAddress::try_from(DirectSelf::ADDRESS)?;
                match context.client().call_json(&address, "{}") {
                    Err(RpcError::DirectSelfCall(_)) => response.write("{}").await,
                    Err(error) => Err(error),
                    Ok(_) => Err(RpcError::InvalidFrameState),
                }
            },
        )
        .expect("register self-calling endpoint");
    let address = RpcAddress::try_from(DirectSelf::ADDRESS).expect("valid address");
    let response = block_on(
        registry
            .client()
            .call_json(&address, "{}")
            .expect("start outer call"),
    )
    .expect("complete outer call");

    assert_eq!(response.as_str(), Ok("{}"));
}

#[test]
fn inconsistent_custom_payload_lengths_are_rejected_on_both_sides() {
    let request_registry = registry();
    request_registry
        .register_json::<EmptyAck, _>(
            "system",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write("{}").await
            },
        )
        .expect("register request endpoint");
    let address = RpcAddress::try_from(EmptyAck::ADDRESS).expect("valid address");
    assert!(matches!(
        block_on(
            request_registry
                .client()
                .call_json(&address, &InconsistentPayload)
                .expect("start inconsistent request")
        ),
        Err(RpcError::InvalidFrameState)
    ));

    let response_registry = registry();
    response_registry
        .register_json::<EmptyAck, _>(
            "system",
            |_context, _request: JsonRef, response: JsonWriter| async move {
                response.write(&InconsistentPayload).await
            },
        )
        .expect("register response endpoint");
    let response = block_on(
        response_registry
            .client()
            .call_json(&address, "{}")
            .expect("start inconsistent response"),
    );
    assert!(matches!(response, Err(RpcError::InvalidFrameState)));
}

#[test]
fn json_clients_report_a_dropped_registry() {
    let registry = registry();
    let client = registry.client();
    let address = RpcAddress::try_from(EmptyAck::ADDRESS).expect("valid address");
    drop(registry);

    assert!(matches!(
        client.json_method_info(&address),
        Err(RpcError::RegistryDropped)
    ));
    assert!(matches!(
        client.call_json(&address, "{}"),
        Err(RpcError::RegistryDropped)
    ));
}
