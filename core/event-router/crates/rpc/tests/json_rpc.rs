#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_rpc::{
    rpc_json, RpcAddress, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcResult, Unary,
};
use futures_lite::future::block_on;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

#[rpc_json]
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
