//! Runs the Event Router schema-bake pipeline end to end: the request schema
//! baked by `build.rs` is embedded on a `#[rpc_dynamic]` method.

use barracuda_event_router::{
    rpc_dynamic, RpcAddress, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
};
use schema_wire::SetLevelRequest;
use serde_json::json;
use static_cell::ConstStaticCell;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, 64, 2>> =
    ConstStaticCell::new(RpcLaneStorage::new());

struct SetLevel;

#[rpc_dynamic]
impl RpcMethod for SetLevel {
    const ADDRESS: &'static str = "session.set_level";
    type Request = SetLevelRequest;
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    // `build.rs` baked `SetLevelRequest.json` into OUT_DIR and set
    // `rpc_schema_baked`, so `#[rpc_dynamic]` embeds it on the method.
    let schema = SetLevel::dynamic()
        .and_then(|dynamic| dynamic.schema())
        .ok_or("the request schema was not baked")?;
    assert!(schema.contains("\"session\""), "{schema}");
    assert!(schema.contains("\"level\""), "{schema}");
    println!("baked request schema:\n{schema}");

    let registry = RpcRegistry::new(RPC_LANES.take());
    registry.register::<SetLevel, _>(
        "system",
        |_context, request: RpcFrame<SetLevelRequest>| async move {
            let request = *request.view()?;
            println!(
                "set_level(session={}, level={})",
                request.session, request.level
            );
            Ok(Ok(()))
        },
    )?;

    let client = registry.client();
    let address = RpcAddress::try_from(SetLevel::ADDRESS)?;
    let ok = client
        .call_json(&address, &json!({ "session": 7, "level": 2 }))
        .await?;
    assert_eq!(ok, json!({ "ok": true, "value": serde_json::Value::Null }));
    println!("call_json ok: {ok}");
    Ok(())
}
