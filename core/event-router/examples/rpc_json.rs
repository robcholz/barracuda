//! Runtime JSON calls to a typed RPC method, through the Event Router facade.
//!
//! Everything comes from `barracuda_event_router`: a method is registered
//! exactly as any typed RPC, `#[rpc_json]` opts it into
//! [`RpcClient::call_json`], and callers reach it by a runtime address string.
//! The bytes on the lane are the real `Request` struct — no JSON travels the
//! router.

use core::cell::Cell;
use std::rc::Rc;

use barracuda_event_router::{
    rpc_json, RpcAddress, RpcContext, RpcError, RpcFrame, RpcHandler, RpcLaneStorage, RpcMethod,
    RpcRegistry, RpcResult, Unary,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, 64, 2>> =
    ConstStaticCell::new(RpcLaneStorage::new());

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

struct SetPermissionLevel;

#[rpc_json]
impl RpcMethod for SetPermissionLevel {
    const ADDRESS: &'static str = "session.set_permission_level";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = SetLevelError;
    type Input = Unary;
    type Output = Unary;
}

fn set_permission_level(applied: Rc<Cell<u32>>) -> impl RpcHandler<SetPermissionLevel> {
    move |_context: RpcContext, request: RpcFrame<SetLevelRequest>| {
        let applied = Rc::clone(&applied);
        async move {
            let request = *request.view()?;
            if request.session == 0 {
                return Ok(Err(SetLevelError::SessionNotOpen));
            }
            if request.level > 2 {
                return Ok(Err(SetLevelError::Denied));
            }
            applied.set(request.level);
            Ok(Ok(()))
        }
    }
}

async fn run() -> RpcResult<()> {
    let registry = RpcRegistry::new(RPC_LANES.take());
    let applied = Rc::new(Cell::new(0));
    registry.register::<SetPermissionLevel, _>(set_permission_level(applied))?;

    let client = registry.client();
    let address = RpcAddress::try_from(SetPermissionLevel::ADDRESS)?;

    // Typed call — unchanged.
    let typed = client
        .call::<SetPermissionLevel>(SetLevelRequest {
            session: 7,
            level: 1,
        })?
        .await?;
    match typed {
        Ok(_) => println!("typed call:    ok"),
        Err(frame) => println!("typed call:    error {:?}", frame.view()?),
    }

    // JSON success: Response is `()`, so the value is null.
    let ok = client
        .call_json(&address, &json!({ "session": 7, "level": 2 }))
        .await?;
    println!("call_json ok:  {ok}");
    assert_eq!(ok, json!({ "ok": true, "value": serde_json::Value::Null }));

    // JSON method errors surface as `ok: false`.
    let denied = client
        .call_json(&address, &json!({ "session": 7, "level": 9 }))
        .await?;
    println!("call_json err: {denied}");
    assert_eq!(denied, json!({ "ok": false, "error": "Denied" }));

    let closed = client
        .call_json(&address, &json!({ "session": 0, "level": 1 }))
        .await?;
    println!("call_json err: {closed}");
    assert_eq!(closed, json!({ "ok": false, "error": "SessionNotOpen" }));

    // A value that does not fit the request is a hard RpcError.
    match client.call_json(&address, &json!({ "session": 7 })).await {
        Ok(value) => println!("call_json bad: unexpected {value}"),
        Err(RpcError::JsonRequestInvalid {
            message_type,
            message,
        }) => {
            println!("call_json bad: rejected ({message_type}: {message})");
        }
        Err(error) => println!("call_json bad: {error}"),
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> RpcResult<()> {
    run().await
}
