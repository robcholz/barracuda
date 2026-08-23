use alloc::rc::Rc;

use barracuda_event_router::{RpcFrame, RpcHandler, RpcMethod, Unary, rpc_dynamic, rpc_message};
use getset::CopyGetters;

use crate::{ScheduleId, SchedulerControl, schedule::ScheduleError};

/// Request accepted by [`Cancel`].
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct CancelRequest {
    /// Schedule to cancel.
    #[getset(get_copy = "pub")]
    id: ScheduleId,
}

impl CancelRequest {
    /// Creates one cancellation request.
    #[must_use]
    pub const fn new(id: ScheduleId) -> Self {
        Self { id }
    }
}

/// Cancellation result.
#[repr(C)]
#[rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct CancelResponse {
    /// Cancelled schedule identifier.
    #[getset(get_copy = "pub")]
    id: ScheduleId,
    /// Number of Events committed before cancellation.
    #[getset(get_copy = "pub")]
    completed_runs: u64,
}

impl CancelResponse {
    const fn new(id: ScheduleId, completed_runs: u64) -> Self {
        Self { id, completed_runs }
    }
}

/// Cancels one live schedule.
pub struct Cancel;

#[rpc_dynamic]
impl RpcMethod for Cancel {
    const ADDRESS: &'static str = "scheduler.cancel";
    type Request = CancelRequest;
    type Response = CancelResponse;
    type Error = ScheduleError;
    type Input = Unary;
    type Output = Unary;
}

/// Builds the reusable handler for [`Cancel`].
pub fn cancel_handler(control: SchedulerControl) -> impl RpcHandler<Cancel> {
    move |_context, request: RpcFrame<CancelRequest>| {
        let result = request.view().copied();
        let shared = Rc::clone(&control.shared);
        async move {
            let request = result?;
            let cancelled = match shared.book.borrow_mut().cancel(&request.id) {
                Ok(cancelled) => cancelled,
                Err(error) => return Ok(Err(error)),
            };
            shared.changed.signal(());
            Ok(Ok(CancelResponse::new(
                request.id,
                cancelled.completed_runs(),
            )))
        }
    }
}
