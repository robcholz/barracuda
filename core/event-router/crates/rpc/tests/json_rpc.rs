#![allow(clippy::expect_used)]
#![allow(clippy::type_complexity)]
#![allow(missing_docs)]

use barracuda_rpc::{
    rpc_dynamic, RpcAddress, RpcContext, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry,
    RpcResult, RpcStream, RpcWire, Streaming, Unary,
};
use futures_lite::future::block_on;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::Cell;
use std::rc::Rc;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

fn registry<const N: usize, const M: usize, const Q: usize>() -> RpcRegistry<N, M, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
    RpcRegistry::new(lanes)
}

// A structured request: real fields, transcoded to the exact struct bytes.
#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
struct SetLevelRequest {
    session: u32,
    level: u32,
}

#[repr(u8)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    TryFromBytes,
)]
enum SetLevelError {
    SessionNotOpen,
    Denied,
}

struct SetLevel;

#[rpc_dynamic]
impl RpcMethod for SetLevel {
    const ADDRESS: &'static str = "session.set_level";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = SetLevelError;
    type Input = Unary;
    type Output = Unary;
}

async fn set_level(
    _context: barracuda_rpc::RpcContext,
    request: RpcFrame<SetLevelRequest>,
) -> RpcResult<Result<(), SetLevelError>> {
    let request = *request.view()?;
    if request.session == 0 {
        return Ok(Err(SetLevelError::SessionNotOpen));
    }
    if request.level > 2 {
        return Ok(Err(SetLevelError::Denied));
    }
    Ok(Ok(()))
}

struct Ping;

#[rpc_dynamic]
impl RpcMethod for Ping {
    const ADDRESS: &'static str = "system.ping";
    type Request = ();
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

async fn ping(
    _context: barracuda_rpc::RpcContext,
    request: RpcFrame<()>,
) -> RpcResult<Result<(), ()>> {
    request.view()?;
    Ok(Ok(()))
}

#[test]
fn call_json_transcodes_a_structured_request_and_void_response() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<SetLevel, _>(set_level)
        .expect("register endpoint");
    let address = RpcAddress::try_from(SetLevel::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let response = client
            .call_json(&address, &json!({ "session": 7, "level": 2 }))
            .await
            .expect("call_json succeeds");
        // Response is `()`, so the value is JSON null.
        assert_eq!(response, json!({ "ok": true, "value": Value::Null }));
    });
}

#[test]
fn json_calls_publish_zero_sized_unit_request_frames() {
    let registry = registry::<1, 1, 1>();
    registry
        .register::<Ping, _>(ping)
        .expect("register endpoint");
    let address = RpcAddress::try_from(Ping::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let response = client
            .call_json(&address, &Value::Null)
            .await
            .expect("unary JSON call succeeds");
        assert_eq!(response, json!({ "ok": true, "value": Value::Null }));
    });
}

// Streaming input mirrors a typed streaming call: every JSON value becomes one
// request frame.
#[repr(C)]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Eq,
    RpcWire,
    TryFromBytes,
)]
struct AppendRequest {
    id: u32,
}

struct Append;

#[rpc_dynamic]
impl RpcMethod for Append {
    const ADDRESS: &'static str = "session.append";
    type Request = AppendRequest;
    type Response = ();
    type Error = ();
    type Input = Streaming;
    type Output = Unary;
}

fn append_handler(
    count: Rc<Cell<u32>>,
) -> impl Fn(
    RpcContext,
    RpcStream<RpcFrame<AppendRequest>>,
)
    -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResult<Result<(), ()>>> + 'static>> {
    move |_context, mut frames| {
        let count = Rc::clone(&count);
        Box::pin(async move {
            while let Some(frame) = frames.next().await {
                count.set(count.get().saturating_add(1));
                let _ = *frame?.view()?;
            }
            Ok(Ok(()))
        })
    }
}

#[test]
fn call_json_stream_writes_every_json_value_as_one_request_frame() {
    let registry = registry::<1, 16, 1>();
    let count = Rc::new(Cell::new(0));
    registry
        .register::<Append, _>(append_handler(Rc::clone(&count)))
        .expect("register endpoint");
    let address = RpcAddress::try_from(Append::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let mut responses = client
            .call_json_stream(
                &address,
                [json!({ "id": 1 }), json!({ "id": 2 }), json!({ "id": 3 })],
            )
            .expect("call_json_stream opens");
        let mut values = Vec::new();
        while let Some(item) = responses.next().await {
            values.push(item.expect("transport ok"));
        }
        assert_eq!(values, vec![Ok(Value::Null)]);
        assert_eq!(count.get(), 3);
    });
}

#[test]
fn call_json_surfaces_a_typed_error_as_ok_false() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<SetLevel, _>(set_level)
        .expect("register endpoint");
    let address = RpcAddress::try_from(SetLevel::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let denied = client
            .call_json(&address, &json!({ "session": 7, "level": 9 }))
            .await
            .expect("call_json succeeds at the transport level");
        assert_eq!(denied, json!({ "ok": false, "error": "Denied" }));

        let closed = client
            .call_json(&address, &json!({ "session": 0, "level": 1 }))
            .await
            .expect("call_json succeeds at the transport level");
        assert_eq!(closed, json!({ "ok": false, "error": "SessionNotOpen" }));
    });
}

#[test]
fn call_json_rejects_a_mismatched_request_value() {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<SetLevel, _>(set_level)
        .expect("register endpoint");
    let address = RpcAddress::try_from(SetLevel::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        // Missing the `level` field: cannot become a SetLevelRequest.
        let error = client
            .call_json(&address, &json!({ "session": 7 }))
            .await
            .expect_err("mismatched request value is rejected");
        assert!(matches!(
            error,
            barracuda_rpc::RpcError::JsonRequestInvalid { .. }
        ));
    });
}
