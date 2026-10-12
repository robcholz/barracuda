#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The QQ provider uploading and sending files against a scripted QQ
//! server, through a real Gateway's HTTP on a loopback network.

mod support;

use std::{boxed::Box, rc::Rc};

use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, ChannelError, GatewayHttp, MediaKind, MediaOpenFuture, MediaSource,
    MessageChannel, MessageTarget, Operation, SendMediaRequest,
};
use barracuda_platform_test::ScriptStep;
use base64::Engine as _;
use futures_lite::stream;
use qq::{QQConfig, QQ};
use serde_json::{json, Value};
use support::{bench, ORIGIN};

const TOKEN: &str = r#"{"access_token":"token-1","expires_in":"7200"}"#;
const UPLOADED: &str = r#"{"file_uuid":"uuid-1","file_info":"INFO-1","ttl":3600}"#;
const MESSAGE: &str = r#"{"id":"qq-message-1"}"#;

fn provider(http: GatewayHttp) -> QQ {
    let mut config = QQConfig::new("app-123", "app-secret");
    config.api_base = ORIGIN.to_owned();
    config.token_url = format!("{ORIGIN}/app/getAppAccessToken");
    QQ::new(http, config)
}

/// A file in memory, read in 7-byte chunks every time it is opened.
struct Memory(Vec<u8>);

impl MediaSource for Memory {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn open(&self) -> MediaOpenFuture<'_> {
        let chunks: Vec<_> = self
            .0
            .chunks(7)
            .map(|chunk| Ok(BinaryChunk::from(chunk.to_vec())))
            .collect();
        Box::pin(async move {
            Ok(Box::pin(stream::iter(chunks)) as barracuda_imessage_gateway_plugin::BinaryStream)
        })
    }
}

fn photo() -> Vec<u8> {
    (0..100_u8).collect()
}

fn source(conversation: &str, filename: &str, mime_type: &str, bytes: Vec<u8>) -> SendMediaRequest {
    SendMediaRequest::source(
        MessageTarget::new("qq", conversation),
        filename,
        mime_type,
        Rc::new(Memory(bytes)),
    )
}

/// The request's head and its body as JSON.
fn split(request: &str) -> (&str, Value) {
    let (head, body) = request.split_once("\r\n\r\n").expect("request body");
    (head, serde_json::from_str(body).expect("JSON body"))
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .expect("Content-Length")
        .trim()
        .parse()
        .expect("a length")
}

#[test]
fn a_file_is_uploaded_as_base64_with_its_length_then_sent_as_a_passive_media_reply() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(200, UPLOADED),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            channel.note_inbound("ROBOT1.0_in", qq::gateway::Scene::Direct);
            let mut request = source("c2c:user-openid", "photo.png", "image/png", photo());
            request.caption = Some("look".into());
            request.reply_to = Some("ROBOT1.0_in".into());

            let receipt = channel
                .send_media(MediaKind::Image, request)
                .await
                .expect("sent");

            assert_eq!(receipt.message_id, "qq-message-1");
            let requests = server.requests();
            assert_eq!(requests.len(), 3);
            let (head, upload) = split(&requests[1]);
            assert!(head.starts_with("POST /v2/users/user-openid/files HTTP/1.1"));
            assert!(head.contains("Authorization: QQBot token-1"));
            assert!(head.contains("Content-Type: application/json"));
            assert!(!head.to_ascii_lowercase().contains("transfer-encoding"));
            let body = requests[1].split_once("\r\n\r\n").unwrap().1;
            assert_eq!(content_length(head), body.len());
            assert_eq!(
                upload,
                json!({
                    "file_type": 1,
                    "srv_send_msg": false,
                    "file_name": "photo.png",
                    "file_data": base64::engine::general_purpose::STANDARD.encode(photo()),
                })
            );
            let (head, message) = split(&requests[2]);
            assert!(head.starts_with("POST /v2/users/user-openid/messages HTTP/1.1"));
            assert_eq!(
                message,
                json!({
                    "msg_type": 7,
                    "media": {"file_info": "INFO-1"},
                    "content": "look",
                    "msg_id": "ROBOT1.0_in",
                    "msg_seq": 1,
                })
            );
        },
    );
}

#[test]
fn groups_upload_to_their_own_endpoint_and_only_silk_audio_is_voice() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(200, UPLOADED),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, UPLOADED),
            ScriptStep::json(200, MESSAGE),
            ScriptStep::json(200, UPLOADED),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            let cases = [
                (MediaKind::Audio, "voice.ogg", "audio/ogg", 4),
                (MediaKind::Audio, "voice.silk", "audio/silk", 3),
                (MediaKind::Video, "clip.mp4", "video/mp4", 2),
            ];
            for (kind, name, mime, _file_type) in cases {
                channel
                    .send_media(kind, source("group:G1", name, mime, b"abc".to_vec()))
                    .await
                    .expect("sent");
            }
            let requests = server.requests();
            for (index, (_kind, name, _mime, file_type)) in cases.into_iter().enumerate() {
                let (head, upload) = split(&requests[1 + 2 * index]);
                assert!(head.starts_with("POST /v2/groups/G1/files HTTP/1.1"));
                assert_eq!(upload["file_type"], file_type);
                assert_eq!(upload["file_name"], name);
                assert_eq!(upload["file_data"], "YWJj");
                let (head, message) = split(&requests[2 + 2 * index]);
                assert!(head.starts_with("POST /v2/groups/G1/messages HTTP/1.1"));
                assert_eq!(
                    message,
                    json!({"msg_type": 7, "media": {"file_info": "INFO-1"}})
                );
            }
        },
    );
}

#[test]
fn an_unauthorized_upload_is_read_again_with_a_new_token() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(401, r#"{"code":11241,"message":"token expired"}"#),
            ScriptStep::json(200, r#"{"access_token":"token-2","expires_in":"7200"}"#),
            ScriptStep::json(200, UPLOADED),
            ScriptStep::json(200, MESSAGE),
        ],
        async |http, server| {
            let channel = provider(http);
            channel
                .send_media(
                    MediaKind::File,
                    source("c2c:user-openid", "a.pdf", "application/pdf", photo()),
                )
                .await
                .expect("sent");
            let requests = server.requests();
            assert_eq!(requests.len(), 5);
            let (head, retried) = split(&requests[3]);
            assert!(head.contains("Authorization: QQBot token-2"));
            assert_eq!(retried["file_type"], 4);
            assert_eq!(
                retried["file_data"],
                base64::engine::general_purpose::STANDARD.encode(photo())
            );
        },
    );
}

#[test]
fn refusals_and_unsendable_files_map_to_channel_errors() {
    bench(
        [
            ScriptStep::json(200, TOKEN),
            ScriptStep::json(400, r#"{"code":850012,"message":"file too large"}"#),
            ScriptStep::json(429, r#"{"code":22009,"message":"limit"}"#),
        ],
        async |http, server| {
            let channel = provider(http);
            let refused = channel
                .send_media(
                    MediaKind::Image,
                    source("c2c:u", "a.png", "image/png", photo()),
                )
                .await;
            assert_eq!(
                refused.err(),
                Some(ChannelError::Platform {
                    code: Some("850012".into()),
                    message: "file too large".into()
                })
            );
            let limited = channel
                .send_media(
                    MediaKind::Image,
                    source("c2c:u", "a.png", "image/png", photo()),
                )
                .await;
            assert_eq!(limited.err(), Some(ChannelError::RateLimited));
            assert_eq!(server.requests().len(), 3);

            // Nothing below reaches QQ.
            let guild = channel
                .send_media(
                    MediaKind::Image,
                    source("channel:42", "a.png", "image/png", photo()),
                )
                .await;
            assert_eq!(
                guild.err(),
                Some(ChannelError::unsupported(Operation::SendImage))
            );
            let unsized_stream = SendMediaRequest::stream(
                MessageTarget::new("qq", "c2c:u"),
                "a.png",
                "image/png",
                Box::pin(stream::iter([Ok(BinaryChunk::from(photo()))])),
            );
            assert!(matches!(
                channel.send_media(MediaKind::Image, unsized_stream).await,
                Err(ChannelError::InvalidRequest { .. })
            ));
            let mut too_large = SendMediaRequest::bytes(
                MessageTarget::new("qq", "c2c:u"),
                "a.bin",
                "application/octet-stream",
                Vec::new(),
            );
            too_large.body = BinaryBody::Stream(Box::pin(stream::empty()));
            too_large.size = Some(qq::media::MAX_UPLOAD_BYTES + 1);
            assert!(matches!(
                channel.send_media(MediaKind::File, too_large).await,
                Err(ChannelError::InvalidRequest { .. })
            ));
            assert_eq!(server.requests().len(), 3);
        },
    );
}
