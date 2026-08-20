#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::future::{pending, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_agent_component::open_session::{
    frames_from_open_session_response, OpenSessionResponse, SessionEventDto,
};
use barracuda_agent_component::session;
use barracuda_agent_runtime::{SessionId, TurnId};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, EventRouter, MemFs,
    RegisterContext, RpcError, RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_gateway_agent_adapter::agent_to_gateway::{agent_to_gateway_handler, AgentToGateway};
use barracuda_gateway_agent_adapter::bind_route::{
    bind_route_handler, frames_from_bind_route_request, BindRoute, BindRouteRequest,
};
use barracuda_gateway_agent_adapter::component::GatewayAgentAdapter;
use barracuda_gateway_agent_adapter::gateway_to_agent::{gateway_to_agent_handler, GatewayToAgent};
use barracuda_message_gateway_component::gateway_message_received::{
    frames_from_gateway_event, GatewayInboundMessage,
};
use barracuda_message_gateway_component::gateway_send::{
    gateway_send_from_frames, GatewayOutboundMessage, GatewaySendRequestFrame,
};
use barracuda_message_gateway_component::route::GatewayRoute;

#[derive(Default)]
struct ResultState {
    append: RefCell<Option<session::append::AppendRequest>>,
    outbound: RefCell<Option<GatewayOutboundMessage>>,
}

struct AdapterClient {
    result: Rc<ResultState>,
}

impl Component<128> for AdapterClient {
    fn register(&mut self, _context: &mut RegisterContext<'_, 128>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<128>) -> ComponentFuture<'a> {
        Box::pin(async move {
            exercise_adapter(context, Rc::clone(&self.result))
                .await
                .map_err(ComponentError::lifecycle)?;
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

async fn exercise_adapter(
    context: RunContext<128>,
    result: Rc<ResultState>,
) -> Result<(), RpcError> {
    let client = context.rpc();
    let session_id = SessionId::new(7);
    let route = GatewayRoute::new("test", "conversation-1");

    let bind_frames =
        frames_from_bind_route_request(&BindRouteRequest::new(route.clone(), session_id))
            .map_err(|_error| RpcError::InvalidFrameState)?;
    client
        .call::<BindRoute>(rpc_input(bind_frames))?
        .await?
        .map_err(|_error| RpcError::InvalidFrameState)?;

    let inbound = GatewayInboundMessage {
        route: route.clone(),
        message_id: "incoming-1".into(),
        text: "hello".into(),
    };
    let mut append_call = client.call::<GatewayToAgent>(rpc_input(
        frames_from_gateway_event(&inbound).map_err(|_error| RpcError::InvalidFrameState)?,
    ))?;
    let mut append_frames = Vec::new();
    while let Some(item) = append_call.next().await {
        let frame = item?.map_err(|_error| RpcError::InvalidFrameState)?;
        append_frames.push(*frame.view()?);
    }
    result.append.replace(Some(
        session::append::append_request_from_frames(append_frames)
            .map_err(|_error| RpcError::InvalidFrameState)?,
    ));

    let responses = [
        OpenSessionResponse::Event {
            session: session_id,
            event: SessionEventDto::OutputDelta {
                text: "agent reply".into(),
            },
        },
        OpenSessionResponse::Event {
            session: session_id,
            event: SessionEventDto::TurnEnded {
                turn: TurnId::new(1),
            },
        },
    ];
    let mut response_frames = Vec::new();
    for response in &responses {
        response_frames.extend(
            frames_from_open_session_response(response)
                .map_err(|_error| RpcError::InvalidFrameState)?,
        );
    }
    let mut gateway_call = client.call::<AgentToGateway>(rpc_input(response_frames))?;
    let mut gateway_frames = Vec::<GatewaySendRequestFrame>::new();
    while let Some(item) = gateway_call.next().await {
        let frame = item?.map_err(|_error| RpcError::InvalidFrameState)?;
        gateway_frames.push(*frame.view()?);
    }
    result.outbound.replace(Some(
        gateway_send_from_frames(gateway_frames).map_err(|_error| RpcError::InvalidFrameState)?,
    ));
    Ok(())
}

fn rpc_input<T: barracuda_event_router::RpcMessage + 'static>(frames: Vec<T>) -> RpcStream<T> {
    RpcStream::new(futures_lite::stream::iter(frames.into_iter().map(Ok)))
}

#[test]
fn adapter_converts_endpoint_contracts_after_explicit_route_binding() {
    let _ = bind_route_handler;
    let _ = gateway_to_agent_handler;
    let _ = agent_to_gateway_handler;
    futures_lite::future::block_on(async {
        let result = Rc::new(ResultState::default());
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 128, 4>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router =
            EventRouter::new(lanes, filesystem, "workflows").expect("build Event Router");
        router
            .load(Box::new(GatewayAgentAdapter::default()))
            .expect("load Adapter Component");
        router
            .load(Box::new(AdapterClient {
                result: Rc::clone(&result),
            }))
            .expect("load Adapter client");

        core::future::poll_fn(|context| {
            if let Poll::Ready(router_result) = Pin::new(&mut router).poll(context) {
                router_result.expect("Event Router remains active");
            }
            if result.outbound.borrow().is_some() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        let append = result.append.borrow();
        let append = append.as_ref().expect("append request");
        assert_eq!(
            append,
            &session::append::AppendRequest::new(
                SessionId::new(7),
                barracuda_agent_runtime::Message::text("hello")
            )
        );

        let outbound = result.outbound.borrow();
        let outbound = outbound.as_ref().expect("gateway outbound message");
        assert_eq!(outbound.route, GatewayRoute::new("test", "conversation-1"));
        assert_eq!(outbound.text, "agent reply");
        assert_eq!(outbound.reply_to.as_deref(), Some("incoming-1"));
    });
}
