//! Category: lane-native JSON RPC registration and invocation.

use core::cell::Cell;
use std::rc::Rc;

use barracuda_rpc::{
    json_schema, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcAddress, RpcLaneStorage,
    RpcRegistry, RpcResult,
};
use futures_lite::future::block_on;
use serde::Deserialize;
use static_cell::ConstStaticCell;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<1, 128, 1>> =
    ConstStaticCell::new(RpcLaneStorage::new());

struct SetMode;

impl JsonRpcSchema for SetMode {
    const ADDRESS: &'static str = "settings.set_mode";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("set_mode", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("set_mode", response);
    const MAX_REQUEST_BYTES: usize = 64;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Deserialize)]
struct SetModeRequest<'a> {
    mode: &'a str,
}

async fn run() -> RpcResult<()> {
    let registry = RpcRegistry::new(RPC_LANES.take());
    let invoked = Rc::new(Cell::new(false));
    let handler_invoked = Rc::clone(&invoked);

    registry.register_json::<SetMode, _>(
        "agent",
        move |_context, request: JsonRef, response: JsonWriter| {
            let invoked = Rc::clone(&handler_invoked);
            async move {
                let request: SetModeRequest<'_> = request.deserialize()?;
                assert_eq!(request.mode, "quiet");
                invoked.set(true);
                response.write("{}").await
            }
        },
    )?;

    let address = RpcAddress::try_from(SetMode::ADDRESS)?;
    let response = registry
        .client()
        .call_json(&address, r#"{"mode":"quiet"}"#)?
        .await?;

    assert_eq!(response.as_str()?, "{}");
    assert!(invoked.get());
    Ok(())
}

fn main() -> RpcResult<()> {
    block_on(run())
}
