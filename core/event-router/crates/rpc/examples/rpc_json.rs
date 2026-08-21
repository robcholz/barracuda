//! Category: runtime JSON calls to existing typed RPC methods.
//!
//! `#[rpc_dynamic]` makes a typed method reachable through
//! [`RpcClient::call_json`], which addresses it by a runtime string and moves
//! the same fixed-layout bytes a typed [`RpcClient::call`] would. The handler is
//! an ordinary typed handler and never sees JSON.

use core::cell::Cell;
use std::rc::Rc;

use barracuda_rpc::{
    rpc_dynamic, RpcAddress, RpcContext, RpcError, RpcFrame, RpcHandler, RpcLaneStorage, RpcMethod,
    RpcRegistry, RpcResult, RpcWire, Unary,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use static_cell::ConstStaticCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, 64, 2>> =
    ConstStaticCell::new(RpcLaneStorage::new());

// DTOs need the zerocopy derives for the wire and the serde derives for JSON.
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
struct AddRequest {
    amount: i64,
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
struct AddResponse {
    total: i64,
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
enum AddError {
    Overflow,
}

// A JSON-callable method: `#[rpc_dynamic]` is the only line that opts it in.
struct CounterAdd;

#[rpc_dynamic]
impl RpcMethod for CounterAdd {
    const ADDRESS: &'static str = "counter.add";
    type Request = AddRequest;
    type Response = AddResponse;
    type Error = AddError;
    type Input = Unary;
    type Output = Unary;
}

// A second method that is NOT annotated: reachable by typed `call`, but not by
// `call_json`.
struct CounterReset;

impl RpcMethod for CounterReset {
    const ADDRESS: &'static str = "counter.reset";
    type Request = AddRequest;
    type Response = AddResponse;
    type Error = AddError;
    type Input = Unary;
    type Output = Unary;
}

fn counter_add(total: Rc<Cell<i64>>) -> impl RpcHandler<CounterAdd> {
    move |_context: RpcContext, request: RpcFrame<AddRequest>| {
        let total = Rc::clone(&total);
        async move {
            let amount = request.view()?.amount;
            match total.get().checked_add(amount) {
                Some(sum) => {
                    total.set(sum);
                    Ok(Ok(AddResponse { total: sum }))
                }
                None => Ok(Err(AddError::Overflow)),
            }
        }
    }
}

fn counter_reset(total: Rc<Cell<i64>>) -> impl RpcHandler<CounterReset> {
    move |_context: RpcContext, _request: RpcFrame<AddRequest>| {
        let total = Rc::clone(&total);
        async move {
            total.set(0);
            Ok(Ok(AddResponse { total: 0 }))
        }
    }
}

async fn run() -> RpcResult<()> {
    let registry = RpcRegistry::new(RPC_LANES.take());
    let total = Rc::new(Cell::new(0));
    registry.register::<CounterAdd, _>(counter_add(Rc::clone(&total)))?;
    registry.register::<CounterReset, _>(counter_reset(total))?;

    let client = registry.client();
    let add = RpcAddress::try_from(CounterAdd::ADDRESS)?;
    let reset = RpcAddress::try_from(CounterReset::ADDRESS)?;

    // (a) The typed call path is unchanged.
    let typed = client.call::<CounterAdd>(AddRequest { amount: 5 })?.await?;
    match typed {
        Ok(frame) => println!("typed call:      total = {}", frame.view()?.total),
        Err(error) => println!("typed call:      error = {:?}", error.view()?),
    }

    // (b) Dynamic JSON call to the same endpoint, addressed by string.
    let success = client.call_json(&add, &json!({ "amount": 3 })).await?;
    println!("call_json ok:    {success}"); // {"ok":true,"value":{"total":8}}
    assert_eq!(success, json!({ "ok": true, "value": { "total": 8 } }));

    // (c) A typed method error surfaces as `ok: false`.
    let overflow = client
        .call_json(&add, &json!({ "amount": i64::MAX }))
        .await?;
    println!("call_json err:   {overflow}"); // {"ok":false,"error":"Overflow"}
    assert_eq!(overflow, json!({ "ok": false, "error": "Overflow" }));

    // (d) A value that does not fit the request is a hard RpcError.
    match client.call_json(&add, &json!({ "wrong": true })).await {
        Ok(value) => println!("call_json bad:   unexpected {value}"),
        Err(error) => println!("call_json bad:   rejected ({error})"),
    }

    // (e) A method without `#[rpc_dynamic]` is not JSON-callable.
    match client.call_json(&reset, &json!({ "amount": 0 })).await {
        Ok(value) => println!("call_json reset: unexpected {value}"),
        Err(RpcError::NotJsonCallable(address)) => {
            println!("call_json reset: not JSON-callable ({address})");
        }
        Err(error) => println!("call_json reset: {error}"),
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> RpcResult<()> {
    run().await
}
