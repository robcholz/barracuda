#![allow(missing_docs)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use std::cell::{Cell, RefCell};
use std::future::Future as _;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, JsonRpcSchema, RegisterContext,
    RpcAddress, RpcLaneStorage, RpcRegistry, RunContext, UnregisterContext,
};
use barracuda_imessage_gateway_component::component::GatewayComponent;
use barracuda_imessage_gateway_component::gateway_message_received::{
    GatewayInboundMessage, GatewayMessageReceived,
};
use barracuda_imessage_gateway_component::gateway_send::{gateway_send_handler, GatewaySend};
use barracuda_imessage_gateway_component::gateway_send_media::{
    GatewaySendMedia, GatewaySendMediaFinished,
};
use barracuda_imessage_gateway_component::gateway_send_stream::{
    GatewaySendStream, GatewaySendStreamFinished,
};
use barracuda_imessage_gateway_component::route::GatewayRoute;
use barracuda_platform_test::install_global_memory_vfs;
use futures_lite::future::block_on;
use futures_lite::StreamExt as _;
use gateway::{
    BinaryBody, ChannelFuture, MediaKind, MessageChannel, MessageGateway, SendMediaRequest,
    SendMessageRequest, SendReceipt, SendStreamField, SendStreamFrame, SendStreamRequest, TextBody,
};

#[test]
fn publishes_three_bounded_json_contracts_and_terminal_event_ids() {
    assert_eq!(GatewaySend::ADDRESS, "gateway.send");
    assert_eq!(GatewaySendStream::ADDRESS, "gateway.send_stream");
    assert_eq!(GatewaySendMedia::ADDRESS, "gateway.send_media");
    for maximum in [
        GatewaySend::MAX_REQUEST_BYTES,
        GatewaySend::MAX_RESPONSE_BYTES,
        GatewaySendStream::MAX_REQUEST_BYTES,
        GatewaySendStream::MAX_RESPONSE_BYTES,
        GatewaySendMedia::MAX_REQUEST_BYTES,
        GatewaySendMedia::MAX_RESPONSE_BYTES,
    ] {
        assert!(maximum <= 512);
    }
    assert!(GatewaySend::REQUEST_SCHEMA
        .as_str()
        .contains("conversation_id"));
    assert!(GatewaySendStream::REQUEST_SCHEMA
        .as_str()
        .contains("finish"));
    assert!(GatewaySendMedia::REQUEST_SCHEMA
        .as_str()
        .contains("content_base64"));
    for schema in [
        GatewaySend::REQUEST_SCHEMA,
        GatewaySendStream::REQUEST_SCHEMA,
        GatewaySendMedia::REQUEST_SCHEMA,
    ] {
        assert!(
            !schema.as_str().contains("maxLength"),
            "the complete RPC lane is the request bound"
        );
    }
    assert_eq!(
        <GatewayMessageReceived as barracuda_event_router::Event>::ID,
        "gateway.message.received"
    );
    assert_eq!(
        <GatewaySendStreamFinished as barracuda_event_router::Event>::ID,
        "gateway.send_stream.finished"
    );
    assert_eq!(
        <GatewaySendMediaFinished as barracuda_event_router::Event>::ID,
        "gateway.send_media.finished"
    );
    for schema in [
        include_str!("../../../schemas/event/gateway_message_received.json"),
        include_str!("../../../schemas/event/gateway_send_stream_finished.json"),
        include_str!("../../../schemas/event/gateway_send_media_finished.json"),
    ] {
        assert!(schema.contains("\"type\""));
        assert!(!schema.contains("maxLength"));
    }
}

#[derive(Default)]
struct State {
    text: RefCell<Vec<String>>,
    stream: RefCell<Vec<SendStreamFrame>>,
    media: RefCell<Vec<u8>>,
    stream_finished: Cell<bool>,
    media_finished: Cell<bool>,
    active_streams: Cell<usize>,
    max_active_streams: Cell<usize>,
    media_was_inline: Cell<bool>,
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
                    message: "complete text required".into(),
                });
            };
            state.text.borrow_mut().push(text);
            Ok(SendReceipt::new("message-1"))
        })
    }

    fn send_stream(&self, mut request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            let active = state.active_streams.get().saturating_add(1);
            state.active_streams.set(active);
            state
                .max_active_streams
                .set(state.max_active_streams.get().max(active));
            while let Some(frame) = request.frames.next().await {
                state.stream.borrow_mut().push(frame?);
            }
            state
                .active_streams
                .set(state.active_streams.get().saturating_sub(1));
            state.stream_finished.set(true);
            Ok(SendReceipt::new("stream-1"))
        })
    }

    fn send_media(
        &self,
        _kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            let BinaryBody::Stream(mut stream) = request.body else {
                return Err(gateway::ChannelError::InvalidRequest {
                    message: "media stream required".into(),
                });
            };
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                state.media_was_inline.set(chunk.is_inline());
                state.media.borrow_mut().extend_from_slice(&chunk);
            }
            state.media_finished.set(true);
            Ok(SendReceipt::new("media-1"))
        })
    }
}

#[test]
fn complete_send_uses_lane_json_and_returns_a_natural_receipt() {
    block_on(async {
        let state = Rc::new(State::default());
        let gateway = MessageGateway::new();
        let _registration = gateway
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 512, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let _rpc = registry
            .register_json::<GatewaySend, _>("*", gateway_send_handler(Rc::new(gateway)))
            .expect("register gateway.send");
        let address = RpcAddress::try_from(GatewaySend::ADDRESS).expect("valid address");
        let text = "x".repeat(400);
        let request = format!(
            r#"{{"channel":"test","conversation_id":"chat","reply_to":"in-1","text":"{text}"}}"#
        );
        let response = registry
            .client()
            .call_json(&address, &request)
            .expect("start call")
            .await
            .expect("complete call");

        assert_eq!(
            response.as_str().expect("response"),
            r#"{"message_id":"message-1"}"#
        );
        assert_eq!(state.text.borrow().as_slice(), &[text]);
    });
}

struct CommandCaller {
    finished: Rc<Cell<bool>>,
}

impl Component<512> for CommandCaller {
    fn name(&self) -> &'static str {
        "command-caller"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, 512>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<512>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = context.rpc().clone();
            let public = client.rpcs_by_visibility("*")?;
            for expected in [
                GatewaySend::ADDRESS,
                GatewaySendStream::ADDRESS,
                GatewaySendMedia::ADDRESS,
            ] {
                if !public.iter().any(|address| address.as_ref() == expected) {
                    return Err(barracuda_event_router::ComponentError::lifecycle(
                        CommandRejected(format!("missing public RPC {expected}")),
                    ));
                }
            }
            if !client.rpcs_by_visibility("agent")?.is_empty() {
                return Err(barracuda_event_router::ComponentError::lifecycle(
                    CommandRejected(String::from("legacy agent visibility is not empty")),
                ));
            }
            for (address, request) in [
                (
                    "gateway.send_stream",
                    r#"{"action":"start","stream_id":"text-1","sequence":0,"channel":"test","conversation_id":"chat"}"#,
                ),
                (
                    "gateway.send_stream",
                    r#"{"action":"start","stream_id":"text-2","sequence":0,"channel":"test","conversation_id":"chat"}"#,
                ),
                (
                    "gateway.send_stream",
                    r#"{"action":"chunk","stream_id":"text-1","sequence":1,"field":"reasoning","boundary":"complete","text":"thinking"}"#,
                ),
                (
                    "gateway.send_stream",
                    r#"{"action":"chunk","stream_id":"text-1","sequence":2,"field":"text","boundary":"complete","text":"answer"}"#,
                ),
                (
                    "gateway.send_stream",
                    r#"{"action":"finish","stream_id":"text-1","sequence":3}"#,
                ),
                (
                    "gateway.send_stream",
                    r#"{"action":"finish","stream_id":"text-2","sequence":1}"#,
                ),
                (
                    "gateway.send_media",
                    r#"{"action":"start","stream_id":"media-1","sequence":0,"channel":"test","conversation_id":"chat","kind":"image","filename":"photo.jpg","mime_type":"image/jpeg"}"#,
                ),
                (
                    "gateway.send_media",
                    r#"{"action":"chunk","stream_id":"media-1","sequence":1,"content_base64":"AAH/gA=="}"#,
                ),
                (
                    "gateway.send_media",
                    r#"{"action":"finish","stream_id":"media-1","sequence":2}"#,
                ),
            ] {
                let address = RpcAddress::try_from(address)
                    .map_err(barracuda_event_router::ComponentError::lifecycle)?;
                loop {
                    let response = client.call_json(&address, request)?.await?;
                    if response.as_str()? == r#"{"error":"busy"}"# {
                        futures_lite::future::yield_now().await;
                        continue;
                    }
                    if response.as_str()?.contains("\"error\"") {
                        return Err(barracuda_event_router::ComponentError::lifecycle(
                            CommandRejected(format!(
                                "{address} rejected {request}: {}",
                                response.as_str()?
                            )),
                        ));
                    }
                    break;
                }
            }
            self.finished.set(true);
            core::future::pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct CommandRejected(String);

#[test]
fn application_streams_are_chunked_acked_and_delivered_without_aggregation() {
    block_on(async {
        install_global_memory_vfs().await.expect("install test VFS");
        let state = Rc::new(State::default());
        let gateway = MessageGateway::new();
        let _registration = gateway
            .register(Rc::new(RecordingChannel(Rc::clone(&state))))
            .expect("register channel");
        let (component, _ingress, runtime) = GatewayComponent::new::<512>(gateway, 2);
        let (inbound, text, media) = runtime.into_parts();
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create router");
        router.load(Box::new(component)).expect("load RPC adapter");
        router.load(Box::new(inbound)).expect("load ingress worker");
        for worker in text {
            router.load(Box::new(worker)).expect("load text worker");
        }
        for worker in media {
            router.load(Box::new(worker)).expect("load media worker");
        }
        let caller_finished = Rc::new(Cell::new(false));
        router
            .load(Box::new(CommandCaller {
                finished: Rc::clone(&caller_finished),
            }))
            .expect("load caller");

        core::future::poll_fn(|context| {
            if let std::task::Poll::Ready(result) = std::pin::Pin::new(&mut router).poll(context) {
                panic!("router stopped before delivery completed: {result:?}");
            }
            if caller_finished.get() && state.stream_finished.get() && state.media_finished.get() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;

        assert_eq!(
            state.stream.borrow().as_slice(),
            &[
                SendStreamFrame::new(
                    SendStreamField::Reasoning,
                    gateway::StreamBoundary::Complete,
                    "thinking",
                ),
                SendStreamFrame::new(
                    SendStreamField::Text,
                    gateway::StreamBoundary::Complete,
                    "answer",
                ),
            ]
        );
        assert_eq!(state.media.borrow().as_slice(), &[0, 1, 255, 128]);
        assert_eq!(state.max_active_streams.get(), 2);
        assert!(state.media_was_inline.get());
        assert!(state
            .stream
            .borrow()
            .iter()
            .all(|frame| frame.text.is_inline()));
    });
}

#[test]
fn inbound_capability_accepts_text_beyond_one_event_lane_for_chunking() {
    block_on(async {
        let (component, ingress, _runtime) = GatewayComponent::new::<512>(MessageGateway::new(), 1);
        let _component = component;
        ingress
            .publish(GatewayInboundMessage {
                route: GatewayRoute::new("test", "chat"),
                message_id: "message-1".into(),
                text: "x".repeat(16 * 1024 + 1),
            })
            .await
            .expect("Event chunking, not a field limit, bounds inbound text");
    });
}
