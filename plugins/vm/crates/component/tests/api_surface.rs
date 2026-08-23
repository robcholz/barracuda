#![allow(missing_docs)]

use barracuda_event_router::{RpcInputMode, RpcMessage, RpcMethod, RpcOutputMode, Streaming};
use barracuda_vm_component::run::{Run, RunError, RunRequestFrame, RunResponseFrame, run_handler};

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
fn vm_run_is_the_only_public_rpc_contract() {
    let _ = run_handler;
    assert_eq!(Run::ADDRESS, "vm.run");
    assert_method::<Run, RunRequestFrame, RunResponseFrame, RunError, Streaming, Streaming>();
    assert_eq!(core::mem::size_of::<RunRequestFrame>(), 64);
    assert_eq!(core::mem::size_of::<RunResponseFrame>(), 64);
    assert_eq!(core::mem::size_of::<RunError>(), 64);
}
