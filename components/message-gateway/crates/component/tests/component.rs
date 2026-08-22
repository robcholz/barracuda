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
use barracuda_message_gateway_component::gateway_message_received::{
    frames_from_gateway_event, gateway_event_from_frames, GatewayInboundMessage,
};
use barracuda_message_gateway_component::gateway_send::{
    frames_from_gateway_send, gateway_send_handler, GatewayOutboundMessage, GatewaySend,
};
use barracuda_message_gateway_component::gateway_send_media::{
    frames_from_gateway_send_media, gateway_send_media_handler, GatewayMediaKind,
    GatewayOutboundMedia, GatewaySendMedia,
};
use barracuda_message_gateway_component::route::GatewayRoute;
use barracuda_message_gateway_component::wire::GatewaySendReceipt;
use futures_lite::future::block_on;
use gateway::{
    BinaryBody, ChannelFuture, MediaKind, MessageChannel, MessageGateway, SendMediaRequest,
    SendMessageRequest, SendReceipt, TextBody,
};

fn assert_gateway_send_response_type()
where
    GatewaySend: barracuda_event_router::RpcMethod<Response = GatewaySendReceipt>,
{
}

#[derive(Default)]
struct State {
    sent: RefCell<Vec<(String, String)>>,
    media: RefCell<Vec<(MediaKind, String, Vec<u8>)>>,
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

    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            let BinaryBody::Bytes(bytes) = request.body else {
                return Err(gateway::ChannelError::InvalidRequest {
                    message: "expected complete media".into(),
                });
            };
            state
                .media
                .borrow_mut()
                .push((kind, request.target.conversation_id, bytes));
            Ok(SendReceipt::new("media-1"))
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
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        router.load(Box::new(component)).expect("load gateway");

        let text = "agent reply 你好 ".repeat(50);
        let message = GatewayOutboundMessage {
            route: GatewayRoute::new("test", "conversation-1"),
            text: text.clone(),
            reply_to: Some("incoming-1".into()),
            kind: gateway::MessageKind::Reply,
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
            &[("conversation-1".into(), text)]
        );
    });
}

#[test]
fn gateway_event_typed_frames_round_trip_long_utf8_text() {
    let message = GatewayInboundMessage {
        route: GatewayRoute::new("telegram", "chat-42").with_thread("topic-7"),
        message_id: "message-100".into(),
        text: "你好 from gateway ".repeat(80),
    };

    let frames = frames_from_gateway_event(&message).expect("encode event");
    assert!(frames.len() > 5);
    let decoded = gateway_event_from_frames(frames).expect("decode event");

    assert_eq!(decoded, message);
}

#[test]
fn gateway_send_media_rpc_delivers_through_registered_channel() {
    let _ = gateway_send_media_handler;
    block_on(async {
        let state = Rc::new(State::default());
        let mut facade = MessageGateway::new();
        facade
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress): (GatewayComponent, GatewayIngress) =
            GatewayComponent::new(facade, 4);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        router.load(Box::new(component)).expect("load gateway");

        let message = GatewayOutboundMedia {
            route: GatewayRoute::new("test", "conversation-1"),
            kind: GatewayMediaKind::Image,
            filename: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            caption: Some("photo".into()),
            reply_to: Some("incoming-1".into()),
            bytes: vec![7_u8; 700],
        };
        router
            .load(Box::new(GatewayMediaCaller {
                message: Some(message),
            }))
            .expect("load caller");

        core::future::poll_fn(|context| {
            let _ = std::pin::Pin::new(&mut router).poll(context);
            if state.media.borrow().is_empty() {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;

        assert_eq!(
            state.media.borrow().as_slice(),
            &[(MediaKind::Image, "conversation-1".into(), vec![7_u8; 700])]
        );
    });
}

struct GatewayCaller {
    message: Option<GatewayOutboundMessage>,
}

struct GatewayMediaCaller {
    message: Option<GatewayOutboundMedia>,
}

impl Component<512> for GatewayMediaCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, 512>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<512>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let Some(message) = self.message.take() else {
                return Ok(());
            };
            let input = RpcStream::new(futures_lite::stream::iter(
                frames_from_gateway_send_media(&message)
                    .map_err(barracuda_event_router::ComponentError::lifecycle)?
                    .into_iter()
                    .map(Ok),
            ));
            context
                .rpc()
                .call::<GatewaySendMedia>(input)?
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

impl Component<512> for GatewayCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, 512>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<512>) -> ComponentFuture<'a> {
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
