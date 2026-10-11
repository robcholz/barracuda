#![allow(
    clippy::arithmetic_side_effects,
    clippy::type_complexity,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

//! The WeChat provider sending files: `getuploadurl` on a scripted iLink
//! service, the encrypted upload on a scripted CDN that keeps every byte it
//! receives, and the message naming the file, through a real Gateway's HTTP
//! on a loopback network.

mod support;

use std::{boxed::Box, cell::RefCell, collections::VecDeque, rc::Rc, string::String, vec::Vec};

use barracuda_imessage_gateway_plugin::{
    BinaryChunk, BinaryStream, ChannelError, MediaKind, MediaOpenFuture, MediaSource,
    MessageChannel, MessageTarget, Operation, SendMediaRequest,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::ScriptStep;
use barracuda_plugin::api::SharedEntropy;
use base64::Engine as _;
use embassy_net::{tcp::TcpSocket, Stack};
use embedded_io_async::Write as _;
use futures_lite::stream;
use serde_json::{json, Value};
use support::{bench_with, API_BASE};
use wechat::{ContextTokens, Wechat, WechatConfig};

/// Port of the scripted CDN.
const CDN_PORT: u16 = 9001;
const CDN_BASE: &str = "http://10.0.0.1:9001/c2c";

std::thread_local! {
    /// The next byte [`Counting`] hands out on this test's thread.
    static NEXT: core::cell::Cell<u8> = const { core::cell::Cell::new(1) };
}

/// Entropy counting up from 1, byte by byte, on each test's thread.
#[derive(Clone)]
struct Counting;

impl Entropy for Counting {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        NEXT.with(|next| {
            for byte in bytes {
                *byte = next.get();
                next.set(next.get().wrapping_add(1));
            }
        });
        Ok(())
    }
}

fn provider(http: barracuda_imessage_gateway_plugin::GatewayHttp) -> Wechat {
    let mut config = WechatConfig::new("bot-token");
    config.api_base = API_BASE.to_owned();
    config.cdn_base = CDN_BASE.to_owned();
    let tokens = Rc::new(ContextTokens::new());
    tokens.remember("wx-user", "ctx-1");
    Wechat::with_context_tokens(http, config, tokens).with_entropy(SharedEntropy::new(Counting))
}

/// A CDN that records each request's head and raw body and answers with
/// the next of its answers.
#[derive(Clone, Default)]
struct Cdn {
    answers: Rc<RefCell<VecDeque<Vec<u8>>>>,
    requests: Rc<RefCell<Vec<(String, Vec<u8>)>>>,
}

impl Cdn {
    fn answering(answers: impl IntoIterator<Item = String>) -> Self {
        let cdn = Self::default();
        cdn.answers
            .borrow_mut()
            .extend(answers.into_iter().map(String::into_bytes));
        cdn
    }

    /// Serves [`CDN_PORT`] with four listeners, forever.
    async fn serve(self, stack: Stack<'static>) {
        embassy_futures::join::join_array(core::array::from_fn::<_, 4, _>(|_| self.listen(stack)))
            .await;
    }

    async fn listen(&self, stack: Stack<'static>) {
        let mut rx = vec![0; 8 * 1024];
        let mut tx = vec![0; 2 * 1024];
        loop {
            let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
            if socket.accept(CDN_PORT).await.is_err() {
                continue;
            }
            if let Some(request) = read_request(&mut socket).await {
                self.requests.borrow_mut().push(request);
                let answer = self.answers.borrow_mut().pop_front().unwrap_or_default();
                let _written = socket.write_all(&answer).await;
                let _flushed = socket.flush().await;
            }
            socket.abort();
            let _flushed = socket.flush().await;
        }
    }
}

async fn read_request(socket: &mut TcpSocket<'_>) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_string();
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while bytes.len() < end + 4 + length {
                let read = socket.read(&mut buffer).await.ok()?;
                if read == 0 {
                    return None;
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            return Some((head, bytes[end + 4..end + 4 + length].to_vec()));
        }
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

fn cdn_answer(status: u16, headers: &str) -> String {
    format!("HTTP/1.1 {status} OK\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
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
        Box::pin(async move { Ok(Box::pin(stream::iter(chunks)) as BinaryStream) })
    }
}

fn photo() -> Vec<u8> {
    (0..100_u8).collect()
}

fn body(request: &str) -> Value {
    let (_head, body) = request.split_once("\r\n\r\n").expect("request body");
    serde_json::from_str(body).expect("JSON body")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn key_from_hex(hex: &str) -> [u8; 16] {
    let mut key = [0; 16];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).unwrap();
    }
    key
}

#[test]
fn an_image_is_encrypted_onto_the_cdn_and_sent_after_its_caption() {
    let cdn = Cdn::answering([cdn_answer(200, "x-encrypted-param: DL/1\r\n")]);
    let serving = cdn.clone();
    bench_with(
        [
            ScriptStep::json(200, r#"{"ret":0,"upload_param":"UP/1+"}"#),
            ScriptStep::json(200, r#"{"ret":0}"#),
            ScriptStep::json(200, r#"{"ret":0}"#),
        ],
        move |stack| serving.serve(stack),
        async |http, server| {
            let channel = provider(http);
            let mut request = SendMediaRequest::source(
                MessageTarget::new("wechat", "wx-user"),
                "photo.png",
                "image/png",
                Rc::new(Memory(photo())),
            );
            request.caption = Some("look".into());

            let receipt = channel
                .send_media(MediaKind::Image, request)
                .await
                .expect("sent");

            let requests = server.requests();
            assert_eq!(requests.len(), 3);
            assert!(requests[0].starts_with("POST /ilink/bot/getuploadurl HTTP/1.1"));
            assert!(requests[0].contains("Authorization: Bearer bot-token"));
            let upload = body(&requests[0]);
            let filekey = upload["filekey"].as_str().expect("filekey");
            let aeskey = upload["aeskey"].as_str().expect("aeskey");
            // Sixteen bytes each from the entropy, as hex.
            assert_eq!(filekey, hex(&(1..=16).collect::<Vec<u8>>()));
            assert_eq!(aeskey, hex(&(17..=32).collect::<Vec<u8>>()));
            assert_eq!(
                upload,
                json!({
                    "filekey": filekey,
                    "media_type": 1,
                    "to_user_id": "wx-user",
                    "rawsize": 100,
                    "rawfilemd5": hex(&barracuda_tls::md5(&photo()).unwrap()),
                    "filesize": 112,
                    "no_need_thumb": true,
                    "aeskey": aeskey,
                    "base_info": {"channel_version": "barracuda-wechat"},
                })
            );

            let uploads = cdn.requests.borrow();
            assert_eq!(uploads.len(), 1);
            let (head, ciphertext) = &uploads[0];
            assert!(
                head.starts_with(&format!(
                    "POST /c2c/upload?encrypted_query_param=UP%2F1%2B&filekey={filekey} HTTP/1.1"
                )),
                "{head}"
            );
            assert!(head.contains("Content-Length: 112"), "{head}");
            assert!(head.contains("Content-Type: application/octet-stream"));
            assert!(!head.to_ascii_lowercase().contains("transfer-encoding"));
            assert!(!head.contains("bot-token"));
            assert_eq!(
                barracuda_tls::aes_128_ecb_decrypt(&key_from_hex(aeskey), ciphertext).unwrap(),
                photo()
            );

            let caption = body(&requests[1]);
            assert!(requests[1].starts_with("POST /ilink/bot/sendmessage HTTP/1.1"));
            assert_eq!(
                caption["msg"]["item_list"],
                json!([{"type": 1, "text_item": {"text": "look"}}])
            );
            let sent = body(&requests[2]);
            assert_eq!(sent["msg"]["to_user_id"], "wx-user");
            assert_eq!(sent["msg"]["context_token"], "ctx-1");
            assert_eq!(sent["msg"]["client_id"], receipt.message_id);
            assert_eq!(
                sent["msg"]["item_list"],
                json!([{"type": 2, "image_item": {
                    "media": {
                        "encrypt_query_param": "DL/1",
                        "aes_key": base64::engine::general_purpose::STANDARD.encode(aeskey),
                        "encrypt_type": 1,
                    },
                    "mid_size": 112,
                }}])
            );
        },
    );
}

#[test]
fn audio_goes_as_a_file_item_and_video_as_a_video_item() {
    let cdn = Cdn::answering([
        cdn_answer(200, "X-Encrypted-Param: DL-A\r\n"),
        cdn_answer(200, "X-Encrypted-Param: DL-V\r\n"),
    ]);
    let serving = cdn.clone();
    bench_with(
        [
            ScriptStep::json(
                200,
                r#"{"ret":0,"upload_full_url":"http://10.0.0.1:9001/full"}"#,
            ),
            ScriptStep::json(200, r#"{"ret":0}"#),
            ScriptStep::json(200, r#"{"ret":0,"upload_param":"UP"}"#),
            ScriptStep::json(200, r#"{"ret":0}"#),
        ],
        move |stack| serving.serve(stack),
        async |http, server| {
            let channel = provider(http);
            let voice = SendMediaRequest::bytes(
                MessageTarget::new("wechat", "wx-user"),
                "voice.ogg",
                "audio/ogg",
                b"0123456789abcdef".to_vec(),
            );
            channel
                .send_media(MediaKind::Audio, voice)
                .await
                .expect("sent");
            let clip = SendMediaRequest::bytes(
                MessageTarget::new("wechat", "wx-user"),
                "clip.mp4",
                "video/mp4",
                b"MP4".to_vec(),
            );
            channel
                .send_media(MediaKind::Video, clip)
                .await
                .expect("sent");

            let requests = server.requests();
            assert_eq!(body(&requests[0])["media_type"], 3);
            assert_eq!(body(&requests[0])["filesize"], 32);
            let file = &body(&requests[1])["msg"]["item_list"][0];
            assert_eq!(file["type"], 4);
            assert_eq!(file["file_item"]["file_name"], "voice.ogg");
            assert_eq!(file["file_item"]["len"], "16");
            assert_eq!(file["file_item"]["media"]["encrypt_query_param"], "DL-A");
            assert_eq!(body(&requests[2])["media_type"], 2);
            let video = &body(&requests[3])["msg"]["item_list"][0];
            assert_eq!(video["type"], 5);
            assert_eq!(video["video_item"]["video_size"], 16);
            assert_eq!(video["video_item"]["media"]["encrypt_query_param"], "DL-V");
            let uploads = cdn.requests.borrow();
            assert!(uploads[0].0.starts_with("POST /full HTTP/1.1"));
            assert_eq!(uploads[0].1.len(), 32);
            assert!(uploads[1]
                .0
                .starts_with("POST /c2c/upload?encrypted_query_param=UP&filekey="));
        },
    );
}

#[test]
fn refusals_and_unsendable_files_map_to_channel_errors() {
    let cdn = Cdn::answering([
        cdn_answer(400, "x-error-message: bad filekey\r\n"),
        cdn_answer(200, ""),
    ]);
    let serving = cdn.clone();
    bench_with(
        [
            ScriptStep::json(200, r#"{"ret":0,"upload_param":"UP"}"#),
            ScriptStep::json(200, r#"{"ret":0,"upload_param":"UP"}"#),
            ScriptStep::json(200, r#"{"ret":-1,"errmsg":"no upload"}"#),
        ],
        move |stack| serving.serve(stack),
        async |http, server| {
            let channel = provider(http.clone());
            let file = || {
                SendMediaRequest::bytes(
                    MessageTarget::new("wechat", "wx-user"),
                    "a.pdf",
                    "application/pdf",
                    photo(),
                )
            };
            assert_eq!(
                channel.send_media(MediaKind::File, file()).await.err(),
                Some(ChannelError::Platform {
                    code: Some("400".into()),
                    message: "bad filekey".into()
                })
            );
            assert!(matches!(
                channel.send_media(MediaKind::File, file()).await,
                Err(ChannelError::Platform { code: None, .. })
            ));
            assert_eq!(
                channel.send_media(MediaKind::File, file()).await.err(),
                Some(ChannelError::Platform {
                    code: Some("-1".into()),
                    message: "no upload".into()
                })
            );
            // Nothing below reaches iLink.
            let streamed = SendMediaRequest::stream(
                MessageTarget::new("wechat", "wx-user"),
                "a.pdf",
                "application/pdf",
                Box::pin(stream::iter([Ok(BinaryChunk::from(photo()))])),
            );
            assert!(matches!(
                channel.send_media(MediaKind::File, streamed).await,
                Err(ChannelError::InvalidRequest { .. })
            ));
            let mut config = WechatConfig::new("bot-token");
            config.api_base = API_BASE.to_owned();
            let without_entropy = Wechat::new(http, config);
            assert_eq!(
                without_entropy
                    .send_media(MediaKind::File, file())
                    .await
                    .err(),
                Some(ChannelError::unsupported(Operation::SendFile))
            );
            assert_eq!(server.requests().len(), 3);
            assert_eq!(cdn.requests.borrow().len(), 2);
        },
    );
}
