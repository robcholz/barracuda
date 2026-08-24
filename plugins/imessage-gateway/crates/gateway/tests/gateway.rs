use std::cell::RefCell;
use std::rc::Rc;

use futures_lite::{future::block_on, stream, StreamExt};
use gateway::{
    BinaryBody, ChannelError, ChannelFuture, DeleteMessageRequest, EditMessageRequest,
    GatewayError, MediaKind, MessageChannel, MessageChannelRegistration, MessageGateway,
    MessageTarget, Operation, ReactRequest, SendMediaRequest, SendMessageRequest, SendReceipt,
    SendStreamField, SendStreamFrame, SendStreamRequest, SetTypingRequest, StreamBoundary,
    StreamError, TextBody,
};

#[derive(Default)]
struct MockState {
    operations: Vec<Operation>,
    media_kinds: Vec<MediaKind>,
    streamed_text: String,
}

struct MockChannel {
    name: &'static str,
    state: Rc<RefCell<MockState>>,
}

impl MockChannel {
    fn receipt() -> Result<SendReceipt, ChannelError> {
        Ok(SendReceipt::new("platform-message"))
    }
}

impl MessageChannel for MockChannel {
    fn channel(&self) -> &str {
        self.name
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            self.state
                .borrow_mut()
                .operations
                .push(Operation::SendMessage);
            if let TextBody::Stream(mut chunks) = request.body {
                while let Some(chunk) = chunks.next().await {
                    self.state.borrow_mut().streamed_text.push_str(&chunk?);
                }
            }
            Self::receipt()
        })
    }

    fn send_media(
        &self,
        kind: MediaKind,
        _request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        self.state.borrow_mut().operations.push(kind.operation());
        self.state.borrow_mut().media_kinds.push(kind);
        Box::pin(async { Self::receipt() })
    }

    fn edit_message(&self, _request: EditMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        self.state
            .borrow_mut()
            .operations
            .push(Operation::EditMessage);
        Box::pin(async { Self::receipt() })
    }

    fn delete_message(&self, _request: DeleteMessageRequest) -> ChannelFuture<'_, ()> {
        self.state
            .borrow_mut()
            .operations
            .push(Operation::DeleteMessage);
        Box::pin(async { Ok(()) })
    }

    fn react(&self, _request: ReactRequest) -> ChannelFuture<'_, ()> {
        self.state.borrow_mut().operations.push(Operation::React);
        Box::pin(async { Ok(()) })
    }

    fn set_typing(&self, _request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        self.state
            .borrow_mut()
            .operations
            .push(Operation::SetTyping);
        Box::pin(async { Ok(()) })
    }
}

fn fixture(
    name: &'static str,
) -> (
    MessageGateway,
    Rc<RefCell<MockState>>,
    MessageChannelRegistration,
) {
    let state = Rc::new(RefCell::new(MockState::default()));
    let provider = Rc::new(MockChannel {
        name,
        state: Rc::clone(&state),
    });
    let gateway = MessageGateway::new();
    let registration = gateway
        .register(provider)
        .expect("register fixture channel");
    (gateway, state, registration)
}

fn target(channel: &str) -> MessageTarget {
    MessageTarget::new(channel, "chat-42")
}

#[test]
fn routes_the_nine_public_operations_to_the_target_channel() {
    block_on(async {
        let (gateway, state, _registration) = fixture("telegram");

        assert!(gateway
            .send_message(SendMessageRequest::text(target("telegram"), "hello"))
            .await
            .is_ok());
        assert!(gateway
            .send_file(SendMediaRequest::bytes(
                target("telegram"),
                "notes.txt",
                "text/plain",
                b"notes".to_vec(),
            ))
            .await
            .is_ok());
        assert!(gateway
            .send_image(SendMediaRequest::bytes(
                target("telegram"),
                "photo.jpg",
                "image/jpeg",
                vec![1],
            ))
            .await
            .is_ok());
        assert!(gateway
            .send_audio(SendMediaRequest::bytes(
                target("telegram"),
                "voice.ogg",
                "audio/ogg",
                vec![2],
            ))
            .await
            .is_ok());
        assert!(gateway
            .send_video(SendMediaRequest::bytes(
                target("telegram"),
                "clip.mp4",
                "video/mp4",
                vec![3],
            ))
            .await
            .is_ok());
        assert!(gateway
            .edit_message(EditMessageRequest::new(
                target("telegram"),
                "message-1",
                "updated",
            ))
            .await
            .is_ok());
        assert!(gateway
            .delete_message(DeleteMessageRequest::new(target("telegram"), "message-1",))
            .await
            .is_ok());
        assert!(gateway
            .react(ReactRequest::new(target("telegram"), "message-1", "👍",))
            .await
            .is_ok());
        assert!(gateway
            .set_typing(SetTypingRequest::new(target("telegram"), true))
            .await
            .is_ok());

        assert_eq!(
            state.borrow().operations,
            vec![
                Operation::SendMessage,
                Operation::SendFile,
                Operation::SendImage,
                Operation::SendAudio,
                Operation::SendVideo,
                Operation::EditMessage,
                Operation::DeleteMessage,
                Operation::React,
                Operation::SetTyping,
            ]
        );
        assert_eq!(
            state.borrow().media_kinds,
            vec![
                MediaKind::File,
                MediaKind::Image,
                MediaKind::Audio,
                MediaKind::Video,
            ]
        );
    });
}

#[test]
fn send_message_passes_an_async_text_stream_to_the_channel() {
    block_on(async {
        let (gateway, state, _registration) = fixture("telegram");
        let chunks = stream::iter([
            Ok("hel".to_owned()),
            Ok("lo ".to_owned()),
            Ok("world".to_owned()),
        ]);

        let result = gateway
            .send_message(SendMessageRequest::stream(
                target("telegram"),
                Box::pin(chunks),
            ))
            .await;

        assert!(result.is_ok());
        assert_eq!(state.borrow().streamed_text, "hello world");
    });
}

#[test]
fn send_message_preserves_a_stream_failure() {
    block_on(async {
        let (gateway, _, _registration) = fixture("telegram");
        let chunks = stream::iter([Err(StreamError::failed("upstream closed"))]);

        let result = gateway
            .send_message(SendMessageRequest::stream(
                target("telegram"),
                Box::pin(chunks),
            ))
            .await;

        assert!(matches!(
            result,
            Err(GatewayError::Channel {
                channel,
                source: ChannelError::Stream(StreamError::Failed { message })
            }) if channel == "telegram" && message == "upstream closed"
        ));
    });
}

#[test]
fn rejects_duplicate_channels_without_replacing_the_first_provider() {
    let (gateway, _, _registration) = fixture("wechat");
    let second = Rc::new(MockChannel {
        name: "wechat",
        state: Rc::new(RefCell::new(MockState::default())),
    });

    let result = gateway.register(second);

    assert!(matches!(
        result,
        Err(GatewayError::DuplicateChannel { channel }) if channel == "wechat"
    ));
}

#[test]
fn dropping_registration_unregisters_the_channel() {
    block_on(async {
        let (gateway, _, registration) = fixture("telegram");
        drop(registration);

        let result = gateway
            .send_message(SendMessageRequest::text(target("telegram"), "hello"))
            .await;

        assert!(matches!(
            result,
            Err(GatewayError::UnknownChannel { channel }) if channel == "telegram"
        ));
    });
}

#[test]
fn reports_an_unknown_target_channel() {
    block_on(async {
        let gateway = MessageGateway::new();

        let result = gateway
            .send_message(SendMessageRequest::text(target("missing"), "hello"))
            .await;

        assert!(matches!(
            result,
            Err(GatewayError::UnknownChannel { channel }) if channel == "missing"
        ));
    });
}

#[test]
fn default_stream_projection_consumes_extras_and_delivers_only_primary_text() {
    block_on(async {
        let (gateway, state, _registration) = fixture("imessage");
        let frames = stream::iter([
            Ok(SendStreamFrame::new(
                SendStreamField::Reasoning,
                StreamBoundary::Complete,
                "hidden thought",
            )),
            Ok(SendStreamFrame::new(
                SendStreamField::Text,
                StreamBoundary::More,
                "hel",
            )),
            Ok(SendStreamFrame::new(
                SendStreamField::ToolOutput,
                StreamBoundary::Complete,
                "hidden tool output",
            )),
            Ok(SendStreamFrame::new(
                SendStreamField::Text,
                StreamBoundary::Complete,
                "lo",
            )),
        ]);

        let receipt = gateway
            .send_stream(SendStreamRequest {
                target: target("imessage"),
                frames: Box::pin(frames),
                reply_to: None,
            })
            .await
            .expect("send projected stream");

        assert_eq!(receipt.message_id, "platform-message");
        assert_eq!(state.borrow().streamed_text, "hello");
    });
}

struct TextOnlyChannel;

impl MessageChannel for TextOnlyChannel {
    fn channel(&self) -> &str {
        "imessage"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Ok(SendReceipt::new("imessage-1")) })
    }
}

#[test]
fn optional_provider_operations_return_typed_unsupported_errors() {
    block_on(async {
        let gateway = MessageGateway::new();
        let _registration = gateway
            .register(Rc::new(TextOnlyChannel))
            .expect("register text-only channel");

        let media = gateway
            .send_file(SendMediaRequest::bytes(
                target("imessage"),
                "notes.txt",
                "text/plain",
                vec![],
            ))
            .await;
        let edit = gateway
            .edit_message(EditMessageRequest::new(
                target("imessage"),
                "message-1",
                "updated",
            ))
            .await;
        let delete = gateway
            .delete_message(DeleteMessageRequest::new(target("imessage"), "message-1"))
            .await;
        let react = gateway
            .react(ReactRequest::new(target("imessage"), "message-1", "❤️"))
            .await;
        let typing = gateway
            .set_typing(SetTypingRequest::new(target("imessage"), true))
            .await;

        assert!(matches!(
            media,
            Err(GatewayError::Channel {
                source: ChannelError::Unsupported {
                    operation: Operation::SendFile
                },
                ..
            })
        ));
        assert!(matches!(
            edit,
            Err(GatewayError::Channel {
                source: ChannelError::Unsupported {
                    operation: Operation::EditMessage
                },
                ..
            })
        ));
        assert!(matches!(
            delete,
            Err(GatewayError::Channel {
                source: ChannelError::Unsupported {
                    operation: Operation::DeleteMessage
                },
                ..
            })
        ));
        assert!(matches!(
            react,
            Err(GatewayError::Channel {
                channel,
                source: ChannelError::Unsupported {
                    operation: Operation::React
                }
            }) if channel == "imessage"
        ));
        assert!(matches!(
            typing,
            Err(GatewayError::Channel {
                source: ChannelError::Unsupported {
                    operation: Operation::SetTyping
                },
                ..
            })
        ));
    });
}

#[test]
fn media_request_can_carry_a_binary_stream() {
    let chunks = stream::iter([Ok(vec![1, 2]), Ok(vec![3, 4])]);
    let request = SendMediaRequest::stream(
        target("wechat"),
        "photo.jpg",
        "image/jpeg",
        Box::pin(chunks),
    );

    assert!(matches!(request.body, BinaryBody::Stream(_)));
}
