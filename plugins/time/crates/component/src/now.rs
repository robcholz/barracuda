use alloc::rc::Rc;
use core::cell::RefCell;

use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary, rpc_dynamic};
pub use barracuda_time_wire::{TimeNow, TimeNowRequest, TimeRpcError};

use crate::ClockState;

/// Reads the network-synchronized real-time clock.
pub struct Now;

#[rpc_dynamic]
impl RpcMethod for Now {
    const ADDRESS: &'static str = "time.now";
    type Request = TimeNowRequest;
    type Response = TimeNow;
    type Error = TimeRpcError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Now`].
pub fn now_handler(state: Rc<RefCell<ClockState>>) -> impl RpcHandler<Now> {
    move |_context, _request: RpcFrame<TimeNowRequest>| {
        let result = state.borrow().now();
        async move { Ok(result) }
    }
}
