#![allow(clippy::expect_used, clippy::indexing_slicing)]

use alloc::rc::Rc;
use core::cell::RefCell;

use futures_lite::{future::block_on, stream};
use gateway::{
    BinaryBody, DeleteMessageRequest, EditMessageRequest, MediaKind, MessageChannel,
    MessageGateway, MessageTarget, ReactRequest, SendMediaRequest, SendMessageRequest,
    SendStreamField, SendStreamFrame, SendStreamRequest, SetTypingRequest, StreamBoundary,
    StreamError,
};
use web::{
    InboundError, InboundFuture, InboundMedia, InboundMessage, InboundMessageSink, MediaPhase,
    MessageBody, Web, WebDelivery, WebEventData, WebService,
};

extern crate alloc;

fn target() -> MessageTarget {
    MessageTarget::new("web", "chat-42")
}

#[test]
fn registers_as_web_and_streams_text_as_start_delta_end() {
    block_on(async {
        let web = Rc::new(Web::<16, 2>::new());
        let gateway = MessageGateway::new();
        let _registration = gateway.register(web.clone()).expect("register Web channel");
        let mut events = web.subscribe("chat-42").expect("subscriber");

        let chunks = stream::iter([Ok("hel".into()), Ok("lo".into())]);
        let receipt = gateway
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await
            .expect("send message");

        let start = events.next().await.expect("start");
        let first = events.next().await.expect("first delta");
        let second = events.next().await.expect("second delta");
        let end = events.next().await.expect("end");

        assert!(matches!(
            start,
            WebDelivery::Event(event)
                if event.id == 1
                    && matches!(event.data, WebEventData::MessageStart { ref message_id, .. } if message_id == &receipt.message_id)
        ));
        assert!(
            matches!(first, WebDelivery::Event(event) if matches!(&event.data, WebEventData::MessageDelta { delta, .. } if delta == "hel"))
        );
        assert!(
            matches!(second, WebDelivery::Event(event) if matches!(&event.data, WebEventData::MessageDelta { delta, .. } if delta == "lo"))
        );
        assert!(
            matches!(end, WebDelivery::Event(event) if matches!(event.data, WebEventData::MessageEnd { .. }))
        );
    });
}

#[test]
fn rich_stream_preserves_extra_frames_beside_primary_text() {
    block_on(async {
        let web = Web::<16, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let frames = stream::iter([
            Ok(SendStreamFrame::new(
                SendStreamField::Reasoning,
                StreamBoundary::Complete,
                "thinking",
            )),
            Ok(SendStreamFrame::new(
                SendStreamField::Text,
                StreamBoundary::Complete,
                "answer",
            )),
        ]);

        let receipt = web
            .send_stream(SendStreamRequest {
                target: target(),
                frames: Box::pin(frames),
                reply_to: Some("incoming-1".into()),
            })
            .await
            .expect("send rich stream");

        let start = events.next().await.expect("start");
        let reasoning = events.next().await.expect("reasoning");
        let text = events.next().await.expect("text");
        let end = events.next().await.expect("end");

        assert!(matches!(
            start,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageStart { message_id, reply_to: Some(reply_to), .. }
                    if message_id == &receipt.message_id && reply_to == "incoming-1")
        ));
        assert!(matches!(
            reasoning,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageExtra {
                    field: SendStreamField::Reasoning,
                    boundary: StreamBoundary::Complete,
                    content,
                    ..
                } if content == "thinking")
        ));
        assert!(matches!(
            text,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageDelta { delta, .. } if delta == "answer")
        ));
        assert!(matches!(
            end,
            WebDelivery::Event(event) if matches!(event.data, WebEventData::MessageEnd { error: None, .. })
        ));
    });
}

#[test]
fn a_failed_text_stream_emits_an_error_end_and_returns_the_failure() {
    block_on(async {
        let web = Web::<8, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let chunks = stream::iter([
            Ok("partial".into()),
            Err(StreamError::failed("upstream closed")),
        ]);

        let result = web
            .send_message(SendMessageRequest::stream(target(), Box::pin(chunks)))
            .await;

        assert!(result.is_err());
        let _start = events.next().await.expect("start");
        let _delta = events.next().await.expect("delta");
        assert!(matches!(
            events.next().await,
            Some(WebDelivery::Event(event))
                if matches!(&event.data, WebEventData::MessageEnd { error: Some(message), .. } if message == "upstream closed")
        ));
    });
}

#[test]
fn maps_all_media_and_mutation_operations_to_events() {
    block_on(async {
        let web = Web::<32, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");

        let media = SendMediaRequest {
            target: target(),
            body: BinaryBody::Stream(Box::pin(stream::iter([Ok(vec![1, 2]), Ok(vec![3])]))),
            filename: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            caption: Some("photo".into()),
            reply_to: Some("parent".into()),
        };
        assert!(web.send_media(MediaKind::Image, media).await.is_ok());
        assert!(web
            .edit_message(EditMessageRequest::new(target(), "web-1", "updated"))
            .await
            .is_ok());
        assert!(web
            .delete_message(DeleteMessageRequest::new(target(), "web-1"))
            .await
            .is_ok());
        assert!(web
            .react(ReactRequest::new(target(), "web-1", "👍"))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .is_ok());

        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::Media { kind: MediaKind::Image, phase: MediaPhase::Start { .. }, .. }))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::Media { phase: MediaPhase::Delta { bytes }, .. } if bytes == &vec![1, 2]))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::Media { phase: MediaPhase::Delta { bytes }, .. } if bytes == &vec![3]))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::Media { phase: MediaPhase::End { error: None }, .. }))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::MessageEdit { text, .. } if text == "updated"))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::MessageDelete { .. }))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::MessageReaction { reaction, .. } if reaction == "👍"))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(&event.data, WebEventData::ConversationTyping { typing: true, .. }))
        );
    });
}

#[test]
fn serializes_stable_sse_names_json_and_base64_media() {
    block_on(async {
        let web = Web::<8, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        assert!(web
            .send_media(
                MediaKind::File,
                SendMediaRequest::bytes(
                    target(),
                    "a.bin",
                    "application/octet-stream",
                    vec![0, 1, 2]
                ),
            )
            .await
            .is_ok());

        let _start = events.next().await.expect("start");
        let delta = events.next().await.expect("delta");
        let frame = delta.to_sse().expect("sse frame");

        assert!(frame.starts_with("id: 2\nevent: message.file\ndata: "));
        assert!(frame.contains("\"phase\":\"delta\""));
        assert!(frame.contains("\"data\":\"AAEC\""));
        assert!(frame.ends_with("\n\n"));
    });
}

#[test]
fn replays_after_last_event_id_and_reports_bounded_history_gaps() {
    block_on(async {
        let web = Web::<2, 2>::new();
        assert!(web
            .send_message(SendMessageRequest::text(target(), "one"))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .is_ok());

        let mut replay = web.subscribe_from("chat-42", Some(1)).expect("subscriber");
        assert!(matches!(
            replay.next().await,
            Some(WebDelivery::Lagged { missed: 1 })
        ));
        assert!(matches!(replay.next().await, Some(WebDelivery::Event(event)) if event.id == 3));
        assert!(matches!(replay.next().await, Some(WebDelivery::Event(event)) if event.id == 4));
    });
}

#[test]
fn subscriptions_are_isolated_by_conversation() {
    block_on(async {
        let web = Web::<8, 2>::new();
        let mut chat_42 = web.subscribe("chat-42").expect("subscriber");
        assert!(web
            .set_typing(SetTypingRequest::new(
                MessageTarget::new("web", "another-chat"),
                true,
            ))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .is_ok());

        assert!(matches!(
            chat_42.next().await,
            Some(WebDelivery::Event(event))
                if event.conversation_id == "chat-42"
                    && matches!(event.data, WebEventData::ConversationTyping { typing: false })
        ));
    });
}

#[derive(Default)]
struct RecordingSink {
    messages: RefCell<Vec<InboundMessage>>,
    media: RefCell<Vec<InboundMedia>>,
}

impl InboundMessageSink for RecordingSink {
    fn receive_message(&self, request: InboundMessage) -> InboundFuture<'_, ()> {
        self.messages.borrow_mut().push(request);
        Box::pin(async { Ok(()) })
    }

    fn receive_media(&self, request: InboundMedia) -> InboundFuture<'_, ()> {
        self.media.borrow_mut().push(request);
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn web_service_maps_rest_json_and_binary_body_to_the_inbound_sink() {
    block_on(async {
        let sink = Rc::new(RecordingSink::default());
        let service = WebService::new(sink.clone());

        let response = service
            .receive_message_json(
                "chat-42",
                br#"{"message_id":"client-1","thread_id":null,"text":"hello","reply_to":"parent"}"#,
            )
            .await
            .expect("valid request");
        assert_eq!(response.message_id, "client-1");
        assert_eq!(sink.messages.borrow()[0].conversation_id, "chat-42");
        assert_eq!(sink.messages.borrow()[0].text, "hello");

        assert!(service
            .receive_media(
                "chat-42",
                MediaKind::Audio,
                "client-2",
                MessageBody::Bytes(vec![4, 5]),
                Some("voice.ogg"),
                Some("audio/ogg"),
                None,
                None,
            )
            .await
            .is_ok());
        assert_eq!(sink.media.borrow().len(), 1);
    });
}

#[test]
fn web_service_rejects_invalid_or_empty_rest_messages_before_the_sink() {
    block_on(async {
        let sink = Rc::new(RecordingSink::default());
        let service = WebService::new(sink.clone());

        let invalid_json = service.receive_message_json("chat", b"{").await;
        let empty = service
            .receive_message_json(
                "chat",
                br#"{"message_id":"client-1","text":"","reply_to":null}"#,
            )
            .await;

        assert!(matches!(
            invalid_json,
            Err(InboundError::InvalidJson { .. })
        ));
        assert!(matches!(empty, Err(InboundError::InvalidRequest { .. })));
        assert!(sink.messages.borrow().is_empty());
    });
}

#[test]
fn complete_text_subscription_limits_and_live_lag_have_explicit_behavior() {
    block_on(async {
        let web = Web::<2, 1>::default();
        assert!(matches!(
            web.subscribe("   "),
            Err(web::SubscribeError::InvalidConversation)
        ));
        let mut events = web.subscribe("chat-42").expect("first subscriber");
        assert!(matches!(
            web.subscribe("chat-42"),
            Err(web::SubscribeError::MaximumSubscribersReached)
        ));
        assert!(web
            .send_message(SendMessageRequest::text(target(), "complete"))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), false))
            .await
            .is_ok());

        assert!(matches!(
            events.next().await,
            Some(WebDelivery::Lagged { missed }) if missed > 0
        ));
        loop {
            let delivery = futures_lite::StreamExt::next(&mut events)
                .await
                .expect("stream trait yields buffered live events");
            if matches!(
                delivery,
                WebDelivery::Event(event)
                    if matches!(event.data, WebEventData::ConversationTyping { typing: false })
            ) {
                break;
            }
        }

        assert!(matches!(
            web.send_message(SendMessageRequest::text(target(), " \t "))
                .await,
            Err(gateway::ChannelError::InvalidRequest { .. })
        ));
    });
}

#[test]
fn binary_and_streaming_media_preserve_success_and_failure_boundaries() {
    block_on(async {
        let web = Web::<16, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let bytes = SendMediaRequest {
            target: target(),
            body: BinaryBody::Bytes(vec![9, 8]),
            filename: None,
            mime_type: None,
            caption: None,
            reply_to: None,
        };
        assert!(web.send_media(MediaKind::Video, bytes).await.is_ok());
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(event.data, WebEventData::Media { phase: MediaPhase::Start { .. }, .. }))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(event.data, WebEventData::Media { phase: MediaPhase::Delta { ref bytes }, .. } if bytes == &vec![9, 8]))
        );
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(event.data, WebEventData::Media { phase: MediaPhase::End { error: None }, .. }))
        );

        let failed = SendMediaRequest {
            target: target(),
            body: BinaryBody::Stream(Box::pin(stream::iter([
                Ok(Vec::new()),
                Err(StreamError::failed("media source failed")),
            ]))),
            filename: None,
            mime_type: None,
            caption: None,
            reply_to: None,
        };
        assert!(web.send_media(MediaKind::Audio, failed).await.is_err());
        let _start = events.next().await.expect("failed media starts");
        assert!(
            matches!(events.next().await, Some(WebDelivery::Event(event)) if matches!(event.data, WebEventData::Media { phase: MediaPhase::End { error: Some(ref message) }, .. } if message == "media source failed"))
        );
    });
}

#[test]
fn every_rich_stream_field_and_mutation_has_a_stable_sse_projection() {
    block_on(async {
        let web = Web::<32, 1>::new();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let fields = [
            (SendStreamField::Text, "text"),
            (SendStreamField::Reasoning, "reasoning"),
            (SendStreamField::EffectResult, "effect_result"),
            (SendStreamField::Notice, "notice"),
            (SendStreamField::Event, "event"),
            (SendStreamField::ToolResultStart, "tool_result_start"),
            (SendStreamField::ToolCallId, "tool_call_id"),
            (SendStreamField::ToolName, "tool_name"),
            (SendStreamField::ToolArguments, "tool_arguments"),
            (SendStreamField::ToolOutput, "tool_output"),
            (SendStreamField::ToolSucceeded, "tool_succeeded"),
            (SendStreamField::ToolFailed, "tool_failed"),
            (SendStreamField::ToolResultEnd, "tool_result_end"),
        ];
        let frames = fields
            .iter()
            .copied()
            .enumerate()
            .map(|(index, (field, _))| {
                Ok(SendStreamFrame::new(
                    field,
                    if index % 2 == 0 {
                        StreamBoundary::More
                    } else {
                        StreamBoundary::Complete
                    },
                    if field == SendStreamField::Text {
                        ""
                    } else {
                        "content"
                    },
                ))
            })
            .collect::<Vec<_>>();
        assert!(web
            .send_stream(SendStreamRequest {
                target: target(),
                frames: Box::pin(stream::iter(frames)),
                reply_to: None,
            })
            .await
            .is_ok());
        let start = events.next().await.expect("rich stream starts");
        assert!(start
            .to_sse()
            .expect("serialize start")
            .contains("message.start"));
        for (_, expected) in fields.iter().skip(1) {
            let frame = events
                .next()
                .await
                .expect("extra frame")
                .to_sse()
                .expect("serialize extra frame");
            assert!(frame.contains(&format!("\"field\":\"{expected}\"")));
            assert!(frame.contains("\"boundary\":"));
        }
        let end = events.next().await.expect("rich stream ends");
        assert!(end.to_sse().expect("serialize end").contains("message.end"));

        assert!(web
            .edit_message(EditMessageRequest::new(target(), "web-1", "new"))
            .await
            .is_ok());
        assert!(web
            .delete_message(DeleteMessageRequest::new(target(), "web-1"))
            .await
            .is_ok());
        assert!(web
            .react(ReactRequest::new(target(), "web-1", "heart"))
            .await
            .is_ok());
        assert!(web
            .set_typing(SetTypingRequest::new(target(), true))
            .await
            .is_ok());
        for expected in [
            "message.edit",
            "message.delete",
            "message.reaction",
            "conversation.typing",
        ] {
            assert!(events
                .next()
                .await
                .expect("mutation event")
                .to_sse()
                .expect("serialize mutation")
                .contains(expected));
        }
        assert_eq!(
            WebDelivery::Lagged { missed: 7 }
                .to_sse()
                .expect("serialize lag marker"),
            "event: stream.lagged\ndata: {\"missed\":7}\n\n"
        );
    });
}

#[derive(Default)]
struct TextOnlySink;

impl InboundMessageSink for TextOnlySink {
    fn receive_message(&self, _request: InboundMessage) -> InboundFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn web_service_validates_media_identity_and_uses_the_default_unsupported_sink() {
    block_on(async {
        let service = WebService::new(Rc::new(TextOnlySink));
        assert!(matches!(
            service
                .receive_media(
                    " ",
                    MediaKind::File,
                    "message",
                    MessageBody::Bytes(vec![]),
                    None,
                    None,
                    None,
                    None,
                )
                .await,
            Err(InboundError::InvalidRequest { .. })
        ));
        assert!(matches!(
            service
                .receive_media(
                    "chat",
                    MediaKind::File,
                    " ",
                    MessageBody::Bytes(vec![]),
                    None,
                    None,
                    None,
                    None,
                )
                .await,
            Err(InboundError::InvalidRequest { .. })
        ));
        assert_eq!(
            service
                .receive_media(
                    "chat",
                    MediaKind::File,
                    "message",
                    MessageBody::Bytes(vec![]),
                    None,
                    None,
                    None,
                    None,
                )
                .await,
            Err(InboundError::Unsupported {
                operation: "receive_media"
            })
        );
    });
}
