#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext, RpcLaneStorage,
    RpcMethod, RpcStream, RunContext, Streaming, Unary, UnregisterContext,
};
use barracuda_imessage_gateway_component::component::{GatewayComponent, GatewayIngress};
use barracuda_imessage_gateway_component::gateway_message_received::{
    frames_from_gateway_event, gateway_event_from_frames, GatewayInboundMessage,
};
use barracuda_imessage_gateway_component::gateway_send::{
    gateway_send_handler, GatewaySend, GatewaySendRequest,
};
use barracuda_imessage_gateway_component::gateway_send_media::{
    frames_from_gateway_send_media, gateway_send_media_handler, GatewayMediaKind,
    GatewayOutboundMedia, GatewaySendMedia,
};
use barracuda_imessage_gateway_component::gateway_send_stream::{
    frames_from_gateway_send_stream, gateway_send_stream_handler, GatewayOutboundStream,
    GatewaySendStream,
};
use barracuda_imessage_gateway_component::route::GatewayRoute;
use barracuda_imessage_gateway_component::wire::GatewaySendReceipt;
use barracuda_platform_test::install_global_memory_vfs;
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use gateway::{
    BinaryBody, ChannelFuture, MediaKind, MessageChannel, MessageGateway, SendMediaRequest,
    SendMessageRequest, SendReceipt, SendStreamField, SendStreamFrame, SendStreamRequest,
    StreamBoundary, TextBody,
};

fn assert_gateway_send_response_type()
where
    GatewaySend: RpcMethod<
        Request = GatewaySendRequest,
        Response = GatewaySendReceipt,
        Input = Unary,
        Output = Unary,
    >,
{
}

fn assert_gateway_send_stream_shape()
where
    GatewaySendStream: RpcMethod<Response = GatewaySendReceipt, Input = Streaming, Output = Unary>,
{
}

#[test]
fn gateway_send_is_dynamic_unary_and_gateway_send_stream_is_typed_streaming() {
    assert_gateway_send_response_type();
    assert_gateway_send_stream_shape();
    assert!(GatewaySend::dynamic().is_some());
    assert!(GatewaySendStream::dynamic().is_none());
}

#[derive(Default)]
struct State {
    sent: RefCell<Vec<(String, String)>>,
    streamed: RefCell<Vec<SendStreamFrame>>,
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

    fn send_stream(&self, mut request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            while let Some(frame) = request.frames.next().await {
                state.streamed.borrow_mut().push(frame?);
            }
            Ok(SendReceipt::new("streamed-1"))
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
fn gateway_send_stream_preserves_text_and_extra_frames_for_the_provider() {
    let _ = gateway_send_stream_handler;
    block_on(async {
        let state = Rc::new(State::default());
        let facade = MessageGateway::new();
        let _registration = facade
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress): (GatewayComponent, GatewayIngress) =
            GatewayComponent::new(facade, 4);
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create router");
        router.load(Box::new(component)).expect("load gateway");

        let message = GatewayOutboundStream {
            route: GatewayRoute::new("test", "conversation-1"),
            reply_to: Some("incoming-1".into()),
            frames: vec![
                SendStreamFrame::new(
                    SendStreamField::Reasoning,
                    StreamBoundary::Complete,
                    "thinking",
                ),
                SendStreamFrame::new(SendStreamField::Text, StreamBoundary::Complete, "answer"),
            ],
        };
        router
            .load(Box::new(GatewayStreamCaller {
                message: Some(message),
            }))
            .expect("load caller");

        core::future::poll_fn(|context| {
            let _ = std::pin::Pin::new(&mut router).poll(context);
            if state.streamed.borrow().is_empty() {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;

        assert_eq!(
            state.streamed.borrow().as_slice(),
            &[
                SendStreamFrame::new(
                    SendStreamField::Reasoning,
                    StreamBoundary::Complete,
                    "thinking",
                ),
                SendStreamFrame::new(SendStreamField::Text, StreamBoundary::Complete, "answer",),
            ]
        );
    });
}

#[test]
fn gateway_send_rpc_delivers_through_registered_channel() {
    let _ = gateway_send_handler;
    block_on(async {
        let state = Rc::new(State::default());
        let facade = MessageGateway::new();
        let _registration = facade
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress): (GatewayComponent, GatewayIngress) =
            GatewayComponent::new(facade, 4);
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create router");
        router.load(Box::new(component)).expect("load gateway");

        let text = "agent reply 你好";
        let message = GatewaySendRequest::with_reply_to(
            &GatewayRoute::new("test", "conversation-1"),
            text,
            Some("incoming-1"),
        )
        .expect("encode request");
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
            &[("conversation-1".into(), text.into())]
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
        let facade = MessageGateway::new();
        let _registration = facade
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress): (GatewayComponent, GatewayIngress) =
            GatewayComponent::new(facade, 4);
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 512, 2>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create router");
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
    message: Option<GatewaySendRequest>,
}

struct GatewayMediaCaller {
    message: Option<GatewayOutboundMedia>,
}

struct GatewayStreamCaller {
    message: Option<GatewayOutboundStream>,
}

impl Component<512> for GatewayStreamCaller {
    fn register(&mut self, _context: &mut RegisterContext<'_, 512>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<512>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let Some(message) = self.message.take() else {
                return Ok(());
            };
            let input = RpcStream::new(futures_lite::stream::iter(
                frames_from_gateway_send_stream(&message)
                    .map_err(barracuda_event_router::ComponentError::lifecycle)?
                    .into_iter()
                    .map(Ok),
            ));
            context
                .rpc()
                .call::<GatewaySendStream>(input)?
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
            context
                .rpc()
                .call::<GatewaySend>(message)?
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
