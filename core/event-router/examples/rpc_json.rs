//! Runtime JSON calls to a `#[rpc_dynamic]` method hosted by one Event Router
//! Component and invoked through another Component's `RunContext::rpc`.

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_event_router::{
    rpc_dynamic, Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext,
    RpcAddress, RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RpcWire, RunContext, Unary,
    UnregisterContext,
};
use barracuda_platform_test::MemFs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

const FRAME_SIZE: usize = 256;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, FRAME_SIZE, 4>> =
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

#[derive(Default)]
struct JsonCallState {
    done: Cell<bool>,
    success: RefCell<Option<Value>>,
    denied: RefCell<Option<Value>>,
    closed: RefCell<Option<Value>>,
    not_dynamic_rejected: Cell<bool>,
    malformed_rejected: Cell<bool>,
    service_unregistered: Cell<bool>,
    caller_unregistered: Cell<bool>,
}

struct PermissionService {
    state: Rc<JsonCallState>,
}

impl Component<FRAME_SIZE> for PermissionService {
    fn register(&mut self, context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        context.register_rpc::<SetPermissionLevel, _>(
            |_context, request: RpcFrame<SetLevelRequest>| async move {
                let request = *request.view()?;
                if request.session == 0 {
                    return Ok(Err(SetLevelError::SessionNotOpen));
                }
                if request.level > 2 {
                    return Ok(Err(SetLevelError::Denied));
                }
                Ok(Ok(()))
            },
        )?;
        context.register_rpc::<CloseSession, _>(
            |_context, _request: RpcFrame<SetLevelRequest>| async move { Ok(Ok(())) },
        )
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.service_unregistered.set(true);
        Ok(())
    }
}

struct JsonCaller {
    state: Rc<JsonCallState>,
}

impl Component<FRAME_SIZE> for JsonCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            let dynamic =
                RpcAddress::try_from(SetPermissionLevel::ADDRESS).map_err(RpcError::from)?;

            let success = client
                .call_json(&dynamic, &json!({ "session": 7, "level": 2 }))
                .await?;
            self.state.success.replace(Some(success));

            let denied = client
                .call_json(&dynamic, &json!({ "session": 7, "level": 9 }))
                .await?;
            self.state.denied.replace(Some(denied));

            let closed = client
                .call_json(&dynamic, &json!({ "session": 0, "level": 1 }))
                .await?;
            self.state.closed.replace(Some(closed));

            if let Err(RpcError::JsonRequestInvalid { .. }) =
                client.call_json(&dynamic, &json!({ "session": 7 })).await
            {
                self.state.malformed_rejected.set(true);
            }

            let typed_only = RpcAddress::try_from(CloseSession::ADDRESS).map_err(RpcError::from)?;
            if let Err(RpcError::NotJsonCallable(_)) = client
                .call_json(&typed_only, &json!({ "session": 7, "level": 1 }))
                .await
            {
                self.state.not_dynamic_rejected.set(true);
            }

            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.caller_unregistered.set(true);
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let state = Rc::new(JsonCallState::default());
    let filesystem = MemFs::new();
    let mut event_router = EventRouter::new(RPC_LANES.take(), filesystem, "workflows")?;

    let service = event_router.load(Box::new(PermissionService {
        state: Rc::clone(&state),
    }))?;
    let caller = event_router.load(Box::new(JsonCaller {
        state: Rc::clone(&state),
    }))?;

    poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut event_router).poll(context) {
            return Poll::Ready(result);
        }
        if state.done.get() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    })
    .await?;

    assert_eq!(
        state.success.borrow().as_ref(),
        Some(&json!({ "ok": true, "value": Value::Null }))
    );
    assert_eq!(
        state.denied.borrow().as_ref(),
        Some(&json!({ "ok": false, "error": "Denied" }))
    );
    assert_eq!(
        state.closed.borrow().as_ref(),
        Some(&json!({ "ok": false, "error": "SessionNotOpen" }))
    );
    assert!(state.not_dynamic_rejected.get());
    assert!(state.malformed_rejected.get());

    event_router.unload(caller)?;
    event_router.unload(service)?;
    assert!(state.caller_unregistered.get());
    assert!(state.service_unregistered.get());

    println!("call_json reached a dynamic Component method through the Event Router");
    Ok(())
}
