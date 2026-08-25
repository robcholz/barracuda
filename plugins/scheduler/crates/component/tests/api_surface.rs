#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_event_router::{Event, RpcInputMode, RpcMessage, RpcMethod, RpcOutputMode, Unary};
use barracuda_scheduler_component::cancel::{
    Cancel, CancelRequest, CancelResponse, cancel_handler,
};
use barracuda_scheduler_component::event::{SchedulerTriggered, Triggered};
use barracuda_scheduler_component::schedule::{
    Schedule, ScheduleError, ScheduleRequest, ScheduleResponse, Trigger, TriggerAt,
    schedule_handler,
};
use serde_json::json;

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
fn scheduler_exposes_two_dynamic_unary_rpcs() {
    let _ = schedule_handler;
    let _ = cancel_handler;
    assert_eq!(Schedule::ADDRESS, "scheduler.schedule");
    assert_eq!(Cancel::ADDRESS, "scheduler.cancel");
    assert_method::<Schedule, ScheduleRequest, ScheduleResponse, ScheduleError, Unary, Unary>();
    assert_method::<Cancel, CancelRequest, CancelResponse, ScheduleError, Unary, Unary>();
    assert!(Schedule::dynamic().is_some());
    assert!(Cancel::dynamic().is_some());
}

#[test]
fn trigger_json_uses_at_for_once_and_interval() {
    let at = TriggerAt::new(2027, 1, 15, 8, 30, 0);
    let once = serde_json::to_value(Trigger::once(at)).expect("serialize once trigger");
    assert_eq!(
        once,
        json!({
            "type":"once",
            "at":{
                "year":2027,
                "month":1,
                "day":15,
                "hour":8,
                "minute":30,
                "second":0
            }
        })
    );

    let expected_interval = Trigger::interval(at, 3_600, 3);
    let interval = serde_json::to_value(expected_interval).expect("serialize interval trigger");
    assert_eq!(
        interval,
        json!({
            "type":"interval",
            "at":{
                "year":2027,
                "month":1,
                "day":15,
                "hour":8,
                "minute":30,
                "second":0
            },
            "every_seconds":3_600,
            "count":3
        })
    );
    let decoded =
        serde_json::from_value::<Trigger>(interval).expect("deserialize interval trigger");
    assert_eq!(decoded, expected_interval);

    assert!(
        serde_json::from_value::<Trigger>(json!({
            "type":"once",
            "starts_at":{
                "year":2027,
                "month":1,
                "day":15,
                "hour":8,
                "minute":30,
                "second":0
            }
        }))
        .is_err()
    );
}

#[test]
fn trigger_event_is_fixed_layout_and_unary() {
    assert_eq!(SchedulerTriggered::ID, "scheduler.triggered");
    assert_eq!(core::mem::size_of::<Triggered>(), 40);
}
