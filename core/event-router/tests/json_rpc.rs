#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_event_router::{
    rpc_dynamic, RpcAddress, RpcContext, RpcError, RpcFrame, RpcLaneStorage, RpcMethod,
    RpcRegistry, RpcResult, RpcWire, Unary,
};
use futures_lite::future::block_on;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

fn registry<const N: usize, const M: usize, const Q: usize>() -> RpcRegistry<N, M, Q> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
    RpcRegistry::new(lanes)
}

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

struct SetPermissionLevel;

#[rpc_dynamic]
impl RpcMethod for SetPermissionLevel {
    const ADDRESS: &'static str = "session.set_permission_level";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = SetLevelError;
    type Input = Unary;
    type Output = Unary;
}

// Not annotated with `#[rpc_dynamic]`: typed calls work, `call_json` does not.
struct CloseSession;

impl RpcMethod for CloseSession {
    const ADDRESS: &'static str = "session.close";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = SetLevelError;
    type Input = Unary;
    type Output = Unary;
}

async fn set_permission_level(
    _context: RpcContext,
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

async fn close_session(
    _context: RpcContext,
    _request: RpcFrame<SetLevelRequest>,
) -> RpcResult<Result<(), SetLevelError>> {
    Ok(Ok(()))
}

fn permission_registry() -> RpcRegistry<1, 16, 1> {
    let registry = registry::<1, 16, 1>();
    registry
        .register::<SetPermissionLevel, _>("system", set_permission_level)
        .expect("register set_permission_level");
    registry
        .register::<CloseSession, _>("system", close_session)
        .expect("register close_session");
    registry
}

#[test]
fn call_json_reaches_a_typed_method_through_the_facade() {
    let registry = permission_registry();
    let address = RpcAddress::try_from(SetPermissionLevel::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let response = client
            .call_json(&address, &json!({ "session": 7, "level": 2 }))
            .await
            .expect("call_json succeeds");
        assert_eq!(response, json!({ "ok": true, "value": Value::Null }));
    });
}

#[test]
fn call_json_maps_typed_errors_to_ok_false() {
    let registry = permission_registry();
    let address = RpcAddress::try_from(SetPermissionLevel::ADDRESS).expect("valid address");
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
fn call_json_rejects_a_mismatched_request() {
    let registry = permission_registry();
    let address = RpcAddress::try_from(SetPermissionLevel::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let error = client
            .call_json(&address, &json!({ "session": 7 }))
            .await
            .expect_err("mismatched request is rejected");
        assert!(matches!(error, RpcError::JsonRequestInvalid { .. }));
    });
}

#[test]
fn call_json_refuses_a_method_without_rpc_dynamic() {
    let registry = permission_registry();
    let address = RpcAddress::try_from(CloseSession::ADDRESS).expect("valid address");
    let client = registry.client();

    block_on(async {
        let error = client
            .call_json(&address, &json!({ "session": 7, "level": 1 }))
            .await
            .expect_err("un-annotated method is not JSON-callable");
        assert!(matches!(error, RpcError::NotJsonCallable(_)));
    });
}
