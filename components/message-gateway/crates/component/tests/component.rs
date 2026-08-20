#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, MemFs, RegisterContext,
    RpcLaneStorage, RpcStream, RunContext, UnregisterContext,
};
use barracuda_message_gateway_component::component::{GatewayComponent, GatewayIngress};
use barracuda_message_gateway_component::gateway_send::{
    frames_from_gateway_send, gateway_send_handler, GatewayOutboundMessage, GatewaySend,
};
use barracuda_message_gateway_component::route::GatewayRoute;
use futures_lite::future::block_on;
use gateway::{
    ChannelFuture, MessageChannel, MessageGateway, SendMessageRequest, SendReceipt, TextBody,
};

fn assert_gateway_send_response_type()
where
    GatewaySend: barracuda_event_router::RpcMethod<Response = ()>,
{
}

#[derive(Default)]
struct State {
    sent: RefCell<Vec<(String, String)>>,
}

struct RecordingChannel(Rc<State>);

impl MessageChannel for RecordingChannel {
    fn channel(&self) -> &str {
        "test"
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            let TextBody::Complete(text) = request.body else {
                return Err(gateway::ChannelError::InvalidRequest {
                    message: "expected complete text".into(),
                });
            };
            state
                .sent
                .borrow_mut()
                .push((request.target.conversation_id, text));
            Ok(SendReceipt::new("sent-1"))
        })
    }
}

#[test]
fn gateway_send_rpc_delivers_through_registered_channel() {
    let _ = gateway_send_handler;
    assert_gateway_send_response_type();
    block_on(async {
        let state = Rc::new(State::default());
        let mut facade = MessageGateway::new();
        facade
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress): (GatewayComponent, GatewayIngress) =
            GatewayComponent::new(facade, 4);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 128, 2>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        router.load(Box::new(component)).expect("load gateway");

        let message = GatewayOutboundMessage {
            route: GatewayRoute::new("test", "conversation-1"),
            text: "agent reply".into(),
            reply_to: Some("incoming-1".into()),
        };
        router
            .load(Box::new(GatewayCaller {
                message: Some(message),
            }))
            .expect("load caller");

        core::future::poll_fn(|context| {
            let _ = std::pin::Pin::new(&mut router).poll(context);
            if state.sent.borrow().is_empty() {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;

        assert_eq!(
            state.sent.borrow().as_slice(),
            &[("conversation-1".into(), "agent reply".into())]
        );
    });
}

struct GatewayCaller {
    message: Option<GatewayOutboundMessage>,
}

impl Component<128> for GatewayCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, 128>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<128>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let Some(message) = self.message.take() else {
                return Ok(());
            };
            let input = RpcStream::new(futures_lite::stream::iter(
                frames_from_gateway_send(&message)
                    .map_err(barracuda_event_router::ComponentError::lifecycle)?
                    .into_iter()
                    .map(Ok),
            ));
            context
                .rpc()
                .call::<GatewaySend>(input)?
                .await?
                .map_err(|_error| {
                    barracuda_event_router::ComponentError::lifecycle(GatewayMethodFailed)
                })?;
            core::future::pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("gateway.send returned a method error")]
struct GatewayMethodFailed;
