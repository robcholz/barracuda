//! WeChat iLink message-channel provider.
#![no_std]

extern crate alloc;

mod context;
mod login;
pub mod media;
mod updates;

use alloc::rc::Rc;
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::Cell;

use barracuda_imessage_gateway_plugin::{
    BinaryBody, BinaryChunk, BinaryStream, ChannelError, ChannelFuture, MediaKind, MediaSource,
    MessageChannel, MessageTarget, SendMediaRequest, SendMessageRequest, SendReceipt, StreamBody,
    StreamError, TextBody,
};
use barracuda_imessage_gateway_plugin::{GatewayHttp, Method, Response};
use barracuda_platform::Entropy as _;
use barracuda_plugin::api::SharedEntropy;
use barracuda_tls::{aes_128_ecb_padded_len, Md5, AES_128_KEY_LEN};
use futures_lite::{stream, StreamExt as _};
use media::{crypto_error, Uploaded};
use serde_json::{json, Value};

pub use context::{ContextTokens, MAX_CONTEXT_TOKENS};
pub use login::{LoginCredentials, LoginError, LoginQrCode, LoginStatus, WechatLogin};
pub use media::{CdnMedia, InboundMedia, DEFAULT_CDN_BASE};
pub use updates::{
    authorization, parse_oversized_updates, parse_updates, updates_body, updates_headers,
    wechat_uin, InboundMessage, Updates, UpdatesError, DEFAULT_LONG_POLL_MS, MAX_UPDATES_BYTES,
    MAX_UPDATES_TAIL_BYTES, SESSION_EXPIRED_CODE, UPDATES_HEADER_BYTES, UPDATES_PATH,
};

/// Default WeChat iLink API base URL.
pub const DEFAULT_API_BASE: &str = "https://ilinkai.weixin.qq.com";
const MAX_TEXT_BYTES: usize = 4_000;
/// `base_info.channel_version` sent with every iLink request.
const CHANNEL_VERSION: &str = "barracuda-wechat";

/// Connection settings matching the headers used by the WeChat iLink bot API.
pub struct WechatConfig {
    pub token: String,
    pub api_base: String,
    pub app_id: String,
    pub client_version: String,
    pub route_tag: Option<String>,
    /// Base64-encoded decimal value used by the `X-WECHAT-UIN` header.
    pub x_wechat_uin: String,
    /// CDN that files are downloaded from and uploaded to, unless iLink
    /// names a full URL.
    pub cdn_base: String,
}

impl WechatConfig {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            api_base: DEFAULT_API_BASE.into(),
            app_id: "bot".into(),
            client_version: "131329".into(),
            route_tag: None,
            x_wechat_uin: "MA==".into(),
            cdn_base: DEFAULT_CDN_BASE.into(),
        }
    }
}

/// Outbound WeChat provider backed by the iLink bot API.
pub struct Wechat {
    http: GatewayHttp,
    config: WechatConfig,
    context_tokens: Rc<ContextTokens>,
    next_client_id: Cell<u64>,
    /// Draws the file keys of uploads; without it files are not sent.
    entropy: Option<SharedEntropy>,
}

impl Wechat {
    pub fn new(http: GatewayHttp, config: WechatConfig) -> Self {
        Self::with_context_tokens(http, config, Rc::default())
    }

    /// Creates a provider whose sends attach the latest `context_token` that
    /// `context_tokens` holds for the recipient, unless the target's
    /// `thread_id` names one.
    pub fn with_context_tokens(
        http: GatewayHttp,
        config: WechatConfig,
        context_tokens: Rc<ContextTokens>,
    ) -> Self {
        Self {
            http,
            config,
            context_tokens,
            next_client_id: Cell::new(0),
            entropy: None,
        }
    }

    /// Lets the provider send files, whose keys it draws from `entropy`.
    #[must_use]
    pub fn with_entropy(mut self, entropy: SharedEntropy) -> Self {
        self.entropy = Some(entropy);
        self
    }

    /// Where to download `media` from; see [`media::download_url`]. The URL
    /// is a credential for the file: never log it.
    #[must_use]
    pub fn download_url(&self, media: &CdnMedia) -> Option<String> {
        media::download_url(&self.config.cdn_base, media)
    }

    /// The connection settings.
    pub fn config(&self) -> &WechatConfig {
        &self.config
    }

    fn next_client_id(&self) -> String {
        let next = self.next_client_id.get().wrapping_add(1).max(1);
        self.next_client_id.set(next);
        format!("espwx-{next:016x}")
    }

    async fn send_text(
        &self,
        target: MessageTarget,
        text: String,
    ) -> Result<SendReceipt, ChannelError> {
        if text.is_empty() {
            return Err(ChannelError::InvalidRequest {
                message: "message text is empty".into(),
            });
        }

        let mut remaining = text.as_str();
        let mut last_receipt = None;
        while !remaining.is_empty() {
            let boundary = utf8_prefix_len(remaining, MAX_TEXT_BYTES);
            let (chunk, rest) = remaining.split_at(boundary);
            last_receipt = Some(self.send_text_chunk(&target, chunk).await?);
            remaining = rest;
        }
        last_receipt.ok_or_else(|| ChannelError::InvalidRequest {
            message: "message text is empty".into(),
        })
    }

    async fn send_text_chunk(
        &self,
        target: &MessageTarget,
        text: &str,
    ) -> Result<SendReceipt, ChannelError> {
        self.send_item(
            target,
            json!({
                "type": media::item_type::TEXT,
                "text_item": { "text": text },
            }),
        )
        .await
    }

    /// Sends one message of the single `item`, with the recipient's
    /// context token.
    async fn send_item(
        &self,
        target: &MessageTarget,
        item: Value,
    ) -> Result<SendReceipt, ChannelError> {
        let client_id = self.next_client_id();
        let mut msg = json!({
            "from_user_id": "",
            "to_user_id": target.conversation_id,
            "client_id": client_id,
            "message_type": 2,
            "message_state": 2,
            "item_list": [item],
        });
        let context_token = target
            .thread_id
            .clone()
            .or_else(|| self.context_tokens.get(&target.conversation_id));
        if let Some(context_token) = context_token {
            let Some(fields) = msg.as_object_mut() else {
                return Err(ChannelError::InvalidRequest {
                    message: "failed to build WeChat message payload".into(),
                });
            };
            fields.insert("context_token".into(), Value::String(context_token));
        }
        let payload = json!({
            "msg": msg,
            "base_info": { "channel_version": CHANNEL_VERSION },
        });
        let bytes = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        self.post_api("sendmessage", bytes.as_slice()).await?;
        Ok(SendReceipt::new(client_id))
    }

    /// Posts `body` to `ilink/bot/<endpoint>` with the bot's headers and
    /// returns the answer's JSON.
    async fn post_api(&self, endpoint: &str, body: &[u8]) -> Result<Value, ChannelError> {
        let url = format!(
            "{}/ilink/bot/{endpoint}",
            self.config.api_base.trim_end_matches('/')
        );
        let authorization = format!("Bearer {}", self.config.token);
        let mut headers = Vec::from([
            ("Content-Type", "application/json"),
            ("iLink-App-Id", self.config.app_id.as_str()),
            (
                "iLink-App-ClientVersion",
                self.config.client_version.as_str(),
            ),
            ("X-WECHAT-UIN", self.config.x_wechat_uin.as_str()),
            ("AuthorizationType", "ilink_bot_token"),
            ("Authorization", authorization.as_str()),
        ]);
        if let Some(route_tag) = &self.config.route_tag {
            headers.push(("SKRouteTag", route_tag));
        }
        let response = self
            .http
            .send(Method::POST, &url, &headers, body)
            .await
            .map_err(|error| ChannelError::Transport {
                message: error.to_string(),
            })?;
        parse_response(response)
    }

    /// Uploads a file to the CDN and sends it; see [`media`].
    async fn send_file(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> Result<SendReceipt, ChannelError> {
        let SendMediaRequest {
            target,
            body,
            filename,
            mime_type,
            caption,
            ..
        } = request;
        let Some(entropy) = self.entropy.as_ref() else {
            return Err(ChannelError::unsupported(kind.operation()));
        };
        let content = match body {
            BinaryBody::Bytes(bytes) => Readable::Bytes(Rc::new(bytes)),
            BinaryBody::Source(source) => Readable::Source(source),
            BinaryBody::Stream(_stream) => {
                // The MD5 comes before the upload, so the file is read twice.
                return Err(ChannelError::InvalidRequest {
                    message: "WeChat uploads need a file that can be read twice; send it by path"
                        .into(),
                });
            }
        };
        let (digest, plaintext_len) = content.digest().await?;
        let ciphertext_len = aes_128_ecb_padded_len(plaintext_len);
        let mut random = [0_u8; AES_128_KEY_LEN * 2];
        entropy.fill(&mut random).map_err(|_unavailable| {
            ChannelError::Stream(StreamError::failed("no entropy for a WeChat file key"))
        })?;
        let (filekey, key) = random.split_at(AES_128_KEY_LEN);
        let filekey = media::hex(filekey);
        let key: [u8; AES_128_KEY_LEN] = key
            .try_into()
            .map_err(|_length| ChannelError::Stream(StreamError::failed("short file key")))?;
        let request = json!({
            "filekey": filekey,
            "media_type": media::upload_media_type(kind),
            "to_user_id": target.conversation_id,
            "rawsize": plaintext_len,
            "rawfilemd5": media::hex(&digest),
            "filesize": ciphertext_len,
            "no_need_thumb": true,
            "aeskey": media::hex(&key),
            "base_info": { "channel_version": CHANNEL_VERSION },
        });
        let bytes = serde_json::to_vec(&request).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        let answer = self.post_api("getuploadurl", &bytes).await?;
        let answer: media::UploadUrl =
            serde_json::from_value(answer).map_err(|error| ChannelError::Platform {
                code: None,
                message: format!("unreadable getuploadurl answer: {error}"),
            })?;
        let Some(upload_url) = media::upload_url(&self.config.cdn_base, &answer, &filekey) else {
            return Err(ChannelError::Platform {
                code: None,
                message: "getuploadurl named no upload URL".into(),
            });
        };
        let download_param = self
            .upload(&upload_url, &content, &key, ciphertext_len)
            .await?;
        let uploaded = Uploaded {
            download_param,
            key,
            plaintext_len,
            ciphertext_len,
        };
        let file_name = filename
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "file.{}",
                    barracuda_imessage_gateway_plugin::mime_extension(
                        mime_type.as_deref().unwrap_or_default()
                    )
                )
            });
        // A caption goes first as a message of its own, as iLink's client
        // sends it.
        if let Some(caption) = caption.filter(|caption| !caption.trim().is_empty()) {
            self.send_text(target.clone(), caption).await?;
        }
        self.send_item(&target, media::sent_item(kind, &uploaded, &file_name))
            .await
    }

    /// Posts the encrypted file to the CDN with a `Content-Length` and
    /// returns the download parameter its `x-encrypted-param` header names.
    async fn upload(
        &self,
        url: &str,
        content: &Readable,
        key: &[u8; AES_128_KEY_LEN],
        ciphertext_len: u64,
    ) -> Result<String, ChannelError> {
        let length =
            usize::try_from(ciphertext_len).map_err(|_overflow| ChannelError::InvalidRequest {
                message: "WeChat file is too large".into(),
            })?;
        let body = StreamBody::new(media::encrypting(content.open().await?, key), length);
        let probe = body.clone();
        // The CDN takes no bot credentials.
        let sent = self
            .http
            .send_reading_headers(
                Method::POST,
                url,
                &[("Content-Type", "application/octet-stream")],
                body,
                &["x-encrypted-param", "x-error-message"],
            )
            .await;
        if let Some(failure) = probe.failure() {
            return Err(ChannelError::Stream(StreamError::failed(failure)));
        }
        let (response, mut headers) = sent.map_err(|error| ChannelError::Transport {
            message: error.to_string(),
        })?;
        let error_message = headers.pop().flatten();
        let download_param = headers.pop().flatten();
        if response.status != 200 {
            return Err(ChannelError::Platform {
                code: Some(response.status.to_string()),
                message: error_message.unwrap_or_else(|| "the WeChat CDN refused the file".into()),
            });
        }
        download_param
            .filter(|param| !param.is_empty())
            .ok_or_else(|| ChannelError::Platform {
                code: None,
                message: "the WeChat CDN answered without x-encrypted-param".into(),
            })
    }

    async fn send_stream(
        &self,
        target: MessageTarget,
        mut stream: barracuda_imessage_gateway_plugin::TextStream,
    ) -> Result<SendReceipt, ChannelError> {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk?);
        }
        self.send_text(target, text).await
    }
}

impl MessageChannel for Wechat {
    fn channel(&self) -> &str {
        "wechat"
    }

    /// Sends text to the target user.
    ///
    /// iLink cannot quote a specific message, so `reply_to` is accepted and
    /// ignored. The send carries the recipient's latest inbound
    /// `context_token`: the target's `thread_id` when set, otherwise the one
    /// the receive loop stored for that user.
    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            match request.body {
                TextBody::Complete(text) => self.send_text(request.target, text).await,
                TextBody::Stream(stream) => self.send_stream(request.target, stream).await,
            }
        })
    }

    /// Uploads the file, encrypted, to the iLink CDN and sends it as an
    /// image, video, or file item (audio goes as a file), after the caption
    /// as a text message of its own. The file must be readable twice (bytes
    /// or a source): its MD5 is part of the upload request.
    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(self.send_file(kind, request))
    }
}

/// A file to send, read once for its digest and again to upload it.
enum Readable {
    Bytes(Rc<Vec<u8>>),
    Source(Rc<dyn MediaSource>),
}

impl Readable {
    async fn open(&self) -> Result<BinaryStream, ChannelError> {
        match self {
            Self::Bytes(bytes) => Ok(Box::pin(stream::once(Ok(BinaryChunk::from(
                bytes.as_ref().clone(),
            ))))),
            Self::Source(source) => Ok(source.open().await?),
        }
    }

    /// The MD5 and length of the content, read once.
    async fn digest(&self) -> Result<([u8; 16], u64), ChannelError> {
        let mut digest = Md5::new().map_err(crypto_error)?;
        let mut len = 0_u64;
        let mut content = self.open().await?;
        while let Some(chunk) = content.next().await {
            let chunk = chunk?;
            digest.update(&chunk).map_err(crypto_error)?;
            len = len.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        }
        Ok((digest.finish().map_err(crypto_error)?, len))
    }
}

fn utf8_prefix_len(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return text.len();
    }
    let mut boundary = max_bytes;
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

fn parse_response(response: Response) -> Result<Value, ChannelError> {
    if response.status == 401 || response.status == 403 {
        return Err(ChannelError::Authentication);
    }
    if response.status == 429 {
        return Err(ChannelError::RateLimited);
    }
    if !(200..300).contains(&response.status) {
        return Err(ChannelError::Platform {
            code: Some(response.status.to_string()),
            message: "WeChat iLink API rejected the request".into(),
        });
    }
    if response.body.is_empty() {
        return Ok(Value::Null);
    }
    let root: Value =
        serde_json::from_slice(&response.body).map_err(|error| ChannelError::Platform {
            code: Some(response.status.to_string()),
            message: error.to_string(),
        })?;
    for key in ["ret", "errcode", "code"] {
        let code = root.get(key).and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let message = root
                .get("errmsg")
                .or_else(|| root.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("WeChat iLink API rejected the request");
            return Err(ChannelError::Platform {
                code: Some(code.to_string()),
                message: message.into(),
            });
        }
    }
    Ok(root)
}
