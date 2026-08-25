#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_event_router::{RpcInputMode, RpcMessage, RpcMethod, RpcOutputMode, Unary};
use barracuda_time_component::now::{Now, TimeNow, TimeNowRequest, TimeRpcError, now_handler};

fn assert_method<M, Request, Response, Error, Input, Output>()
where
    M: RpcMethod<
            Request = Request,
            Response = Response,
            Error = Error,
            Input = Input,
            Output = Output,
        >,
    Request: RpcMessage,
    Response: RpcMessage,
    Error: RpcMessage,
    Input: RpcInputMode<Request>,
    Output: RpcOutputMode<Response, Error>,
{
}

#[test]
fn time_now_is_dynamic_fixed_layout_and_schema_baked() {
    let _ = now_handler;
    assert_eq!(Now::ADDRESS, "time.now");
    assert_method::<Now, TimeNowRequest, TimeNow, TimeRpcError, Unary, Unary>();
    let schema = Now::dynamic()
        .and_then(|dynamic| dynamic.schema())
        .expect("time.now request schema is baked");
    assert!(schema.contains("TimeNowRequest"));
    assert!(schema.contains("\"type\": \"object\""));
    assert!(!schema.contains("reserved"));
    assert_eq!(core::mem::size_of::<TimeNow>(), 8);
}
