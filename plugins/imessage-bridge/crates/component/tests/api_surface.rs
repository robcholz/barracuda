#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_event_router::JsonRpcSchema;
use barracuda_imessage_bridge_component::{ToAgent, ToGateway};

#[test]
fn workflow_rpcs_have_stable_addresses_and_schemas() {
    assert_eq!(ToAgent::ADDRESS, "imessage_bridge.to_agent");
    assert_eq!(ToGateway::ADDRESS, "imessage_bridge.to_gateway");
    assert_eq!(
        ToAgent::REQUEST_SCHEMA.as_str(),
        include_str!("../../../schemas/rpc/to_agent/request.json")
    );
    assert_eq!(
        ToGateway::RESPONSE_SCHEMA.as_str(),
        include_str!("../../../schemas/rpc/to_gateway/response.json")
    );
    assert_eq!(ToAgent::MAX_REQUEST_BYTES, 512);
    assert_eq!(ToGateway::MAX_RESPONSE_BYTES, 512);
}
