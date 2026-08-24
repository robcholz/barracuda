#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_agent_component::open_session::{SessionEventDto, ToolOutputDto};
use barracuda_agent_runtime::ToolCall;
use barracuda_event_router::{Event, RpcMethod, Streaming};
use barracuda_gateway_agent_component::respond::{
    gateway_frames_from_session_event, GatewayAgentRespond,
};
use barracuda_imessage_gateway_component::gateway_message_received::{
    GatewayEventFrame, GatewayMessageReceived,
};
use barracuda_imessage_gateway_component::gateway_send_stream::{
    GatewaySendStreamField, GatewaySendStreamRequestFrame,
};

fn assert_mapper_shape()
where
    GatewayAgentRespond: RpcMethod<
        Request = GatewayEventFrame,
        Response = GatewaySendStreamRequestFrame,
        Input = Streaming,
        Output = Streaming,
    >,
{
}

#[test]
fn respond_is_the_streaming_event_to_gateway_mapper() {
    assert_mapper_shape();
    assert_eq!(GatewayAgentRespond::ADDRESS, "gateway_agent.respond");
    assert_eq!(
        <GatewayMessageReceived as Event>::ID,
        "gateway.message.received"
    );
}

#[test]
fn tool_result_maps_to_one_structured_extra_frame_sequence_without_truncation() {
    let event = SessionEventDto::ToolResult {
        call: ToolCall {
            id: "call-42".into(),
            name: "lookup".into(),
            arguments_json: format!(r#"{{"query":"{}"}}"#, "问题".repeat(300)),
        },
        output: ToolOutputDto {
            content: "结果".repeat(500),
            ok: true,
        },
    };

    let frames = gateway_frames_from_session_event(&event).expect("map tool result");

    assert!(frames.len() > 8);
    assert_eq!(
        frames.first().map(GatewaySendStreamRequestFrame::field),
        Some(GatewaySendStreamField::ToolResultStart)
    );
    assert_eq!(
        frames.last().map(GatewaySendStreamRequestFrame::field),
        Some(GatewaySendStreamField::ToolResultEnd)
    );
    assert!(frames
        .iter()
        .any(|frame| frame.field() == GatewaySendStreamField::ToolSucceeded));
}
