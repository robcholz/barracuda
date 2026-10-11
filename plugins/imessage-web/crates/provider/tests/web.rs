#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use alloc::rc::Rc;
use core::cell::RefCell;

use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, BinaryStream, ChannelError, DeleteMessageRequest, EditMessageRequest,
    MediaKind, MediaOpenFuture, MediaSource, MessageChannel, MessageGateway, MessageTarget,
    ReactRequest, SendMediaRequest, SendMessageRequest, SendStreamEvent, SendStreamRequest,
    SetTypingRequest, StreamError,
};
use barracuda_plugin::api::SharedEntropy;
use futures_lite::{future::block_on, stream, StreamExt};
use web::{MediaSpool, SpoolFuture, Web, WebDelivery, WebEventData, MAX_KEPT};

extern crate alloc;

fn target() -> MessageTarget {
    MessageTarget::new("web", "chat-42")
}

/// Bytes of known length, read again on every open.
struct Memory(Vec<u8>);

impl MediaSource for Memory {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn open(&self) -> MediaOpenFuture<'_> {
        let bytes = self.0.clone();
        Box::pin(async move {
            Ok(Box::pin(stream::iter([Ok(BinaryChunk::from(bytes))])) as BinaryStream)
        })
    }
}

/// What a [`Spool`] wrote, in order: the kind, MIME type, name and bytes.
type Spooled = Rc<RefCell<Vec<(MediaKind, String, Option<String>, Vec<u8>)>>>;

/// Spools into memory and records each body, as the Plugin's spool writes to
/// the Gateway's storage.
#[derive(Default)]
struct Spool(Spooled);

impl MediaSpool for Spool {
    fn spool(
        &self,
        kind: MediaKind,
        mime_type: String,
        filename: Option<String>,
        mut body: BinaryStream,
    ) -> SpoolFuture<'_> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk?);
            }
            self.0
                .borrow_mut()
                .push((kind, mime_type, filename, bytes.clone()));
            Ok(Rc::new(Memory(bytes)) as Rc<dyn MediaSource>)
        })
    }
}

fn channel<const CAP: usize, const SUBS: usize>() -> (Web<CAP, SUBS>, Spooled) {
    let spool = Spool::default();
    let spooled = Rc::clone(&spool.0);
    (
        Web::new(Rc::new(spool), SharedEntropy::unavailable()),
        spooled,
    )
}

async fn read_all(source: &dyn MediaSource) -> Vec<u8> {
    let mut stream = source.open().await.expect("open");
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.expect("chunk"));
    }
    bytes
}

#[test]
fn registers_as_web_and_streams_text_as_start_delta_end() {
    block_on(async {
        let web = Rc::new(channel::<16, 2>().0);
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
fn rich_stream_preserves_every_semantic_event() {
    block_on(async {
        let web = channel::<16, 1>().0;
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let stream_events = stream::iter([
            Ok(SendStreamEvent::new(
                "session-1",
                1,
                "reasoning_delta",
                r#"{"text":"thinking"}"#,
            )),
            Ok(SendStreamEvent::new(
                "session-1",
                2,
                "output_delta",
                r#"{"text":"answer"}"#,
            )),
        ]);

        let receipt = web
            .send_stream(SendStreamRequest {
                target: target(),
                events: Box::pin(stream_events),
                reply_to: Some("incoming-1".into()),
            })
            .await
            .expect("send rich stream");

        let start = events.next().await.expect("start");
        let reasoning = events.next().await.expect("reasoning");
        let text = events.next().await.expect("text");
        let end = events.next().await.expect("end");
        let reasoning_sse = reasoning.to_sse().expect("serialize semantic event");

        assert!(matches!(
            start,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageStart { message_id, reply_to: Some(reply_to), .. }
                    if message_id == &receipt.message_id && reply_to == "incoming-1")
        ));
        assert!(matches!(
            reasoning,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageEvent {
                    event,
                    ..
                } if event.event_type == "reasoning_delta" && event.payload.as_str() == r#"{"text":"thinking"}"#)
        ));
        assert!(reasoning_sse.contains("event: message.event"));
        assert!(reasoning_sse.contains(r#""session":"session-1""#));
        assert!(reasoning_sse.contains(r#""sequence":1"#));
        assert!(reasoning_sse.contains(r#""type":"reasoning_delta""#));
        assert!(reasoning_sse.contains(r#""payload":{"text":"thinking"}"#));
        assert!(matches!(
            text,
            WebDelivery::Event(event)
                if matches!(&event.data, WebEventData::MessageEvent { event, .. }
                    if event.event_type == "output_delta" && event.payload.as_str() == r#"{"text":"answer"}"#)
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
        let web = channel::<8, 1>().0;
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
fn maps_mutation_operations_to_events() {
    block_on(async {
        let web = channel::<32, 1>().0;
        let mut events = web.subscribe("chat-42").expect("subscriber");

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

/// The kept file behind a media event's URL.
fn kept<const CAP: usize, const SUBS: usize>(
    web: &Web<CAP, SUBS>,
    url: &str,
) -> Option<web::KeptMedia> {
    web.media()
        .kept(url.strip_prefix(web::MEDIA_PATH).expect("a media URL"))
}

#[test]
fn every_body_becomes_one_media_event_naming_a_url() {
    block_on(async {
        let (web, spooled) = channel::<16, 2>();
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let source: Rc<dyn MediaSource> = Rc::new(Memory(vec![4, 5, 6]));

        let mut photo =
            SendMediaRequest::source(target(), "photo.jpg", "image/jpeg", Rc::clone(&source));
        photo.caption = Some("look".into());
        photo.reply_to = Some("web-in-1".into());
        let photo = web
            .send_media(MediaKind::Image, photo)
            .await
            .expect("send a source");
        let bytes = web
            .send_media(
                MediaKind::File,
                SendMediaRequest::bytes(target(), "a.bin", "", vec![0, 1, 2]),
            )
            .await
            .expect("send bytes");
        let streamed = web
            .send_media(
                MediaKind::Audio,
                SendMediaRequest::stream(
                    target(),
                    "voice.ogg",
                    "audio/ogg",
                    Box::pin(stream::iter([
                        Ok(BinaryChunk::from(vec![7, 8])),
                        Ok(BinaryChunk::from(vec![9])),
                    ])),
                ),
            )
            .await
            .expect("send a stream");

        // a source is kept as it is; bytes and streams are spooled, nothing else
        assert_eq!(
            *spooled.borrow(),
            vec![
                (
                    MediaKind::File,
                    "application/octet-stream".into(),
                    Some("a.bin".into()),
                    vec![0, 1, 2]
                ),
                (
                    MediaKind::Audio,
                    "audio/ogg".into(),
                    Some("voice.ogg".into()),
                    vec![7, 8, 9]
                ),
            ]
        );
        let mut seen = Vec::new();
        for receipt in [photo, bytes, streamed] {
            let Some(WebDelivery::Event(event)) = events.next().await else {
                panic!("one event per media message");
            };
            let WebEventData::Media {
                message_id,
                url,
                size,
                ..
            } = &event.data
            else {
                panic!("a media event, not {:?}", event.data);
            };
            assert_eq!(message_id, &receipt.message_id);
            assert_eq!(*size, 3);
            seen.push((event.clone(), url.clone()));
        }
        let image = kept(&web, &seen[0].1).expect("the photo is kept");
        assert!(Rc::ptr_eq(&image.source, &source), "a source is not copied");
        assert_eq!(
            read_all(&*kept(&web, &seen[2].1).unwrap().source).await,
            [7, 8, 9]
        );
        let frame = WebDelivery::Event(Rc::clone(&seen[0].0)).to_sse().unwrap();
        assert!(frame.starts_with("id: 1\nevent: message.image\ndata: "));
        assert!(frame.contains(r#""caption":"look","filename":"photo.jpg","message_id":"web-1","mime_type":"image/jpeg","reply_to":"web-in-1","size":3,"url":"/api/web/media/00000000-0""#));
        let frame = WebDelivery::Event(Rc::clone(&seen[1].0)).to_sse().unwrap();
        assert!(frame.contains("event: message.file"));
        assert!(frame.contains(r#""mime_type":"application/octet-stream""#));

        // replay holds the same three small events, never the bytes
        let mut replay = web.subscribe_from("chat-42", Some(0)).expect("replay");
        for _ in 0..3 {
            let Some(delivery) = replay.next().await else {
                panic!("replayed");
            };
            let frame = delivery.to_sse().unwrap();
            assert!(frame.len() < 256, "{}", frame.as_str());
            assert!(!frame.contains("\"data\""));
        }
    });
}

/// Fails to open, as a file that left the cache does.
struct Failing;

impl MediaSource for Failing {
    fn len(&self) -> u64 {
        1
    }

    fn open(&self) -> MediaOpenFuture<'_> {
        Box::pin(async { Err(StreamError::failed("gone")) })
    }
}

/// Fails every body, as a full cache does.
struct Refusing;

impl MediaSpool for Refusing {
    fn spool(
        &self,
        _kind: MediaKind,
        _mime_type: String,
        _filename: Option<String>,
        _body: BinaryStream,
    ) -> SpoolFuture<'_> {
        Box::pin(async {
            Err(ChannelError::Transport {
                message: "out of space".into(),
            })
        })
    }
}

#[test]
fn a_failed_spool_sends_nothing_and_kept_files_are_bounded() {
    block_on(async {
        let web = Web::<8, 1>::new(Rc::new(Refusing), SharedEntropy::unavailable());
        let mut events = web.subscribe("chat-42").expect("subscriber");
        let refused = web
            .send_media(
                MediaKind::File,
                SendMediaRequest::bytes(target(), "a.bin", "application/zip", vec![1]),
            )
            .await;
        assert!(matches!(refused, Err(ChannelError::Transport { .. })));

        let mut urls = Vec::new();
        for _ in 0..=MAX_KEPT {
            web.send_media(
                MediaKind::File,
                SendMediaRequest {
                    body: BinaryBody::Source(Rc::new(Failing)),
                    ..SendMediaRequest::bytes(target(), "a.bin", "application/zip", Vec::new())
                },
            )
            .await
            .expect("a source is sent without being opened");
            let Some(WebDelivery::Event(event)) = events.next().await else {
                panic!("an event");
            };
            let WebEventData::Media { url, .. } = &event.data else {
                panic!("a media event");
            };
            urls.push(url.clone());
        }
        assert!(kept(&web, &urls[0]).is_none(), "the oldest is evicted");
        assert!(kept(&web, &urls[1]).is_some());
        assert!(kept(&web, &urls[MAX_KEPT]).is_some());
    });
}

#[test]
fn replays_after_last_event_id_and_reports_bounded_history_gaps() {
    block_on(async {
        let web = channel::<2, 2>().0;
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
        let web = channel::<8, 2>().0;
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
