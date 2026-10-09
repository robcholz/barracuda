//! Inkbox iMessage API message-channel provider.
#![no_std]

extern crate alloc;

mod signup;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
};

use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, MediaKind, MessageChannel, ReactRequest, SendMediaRequest,
    SendMessageRequest, SendReceipt, SetTypingRequest, StreamError, TextBody,
};
use barracuda_imessage_gateway_plugin::{Method, Multipart, RequestBody, Response};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;
use serde_json::{json, Value};

pub use signup::{InkboxSignup, SignupAccount, SignupError, SignupRequest};

/// Gateway channel name this provider registers.
pub const CHANNEL: &str = "inkbox";

/// Production Inkbox service origin, without the `/api` suffix.
pub const DEFAULT_API_BASE: &str = "https://inkbox.ai";

/// Connection settings for the Inkbox iMessage API.
pub struct InkboxConfig {
    /// Identity-scoped Inkbox API key.
    pub api_key: String,
    /// Inkbox agent identity UUID used for outbound requests.
    pub identity_id: String,
    /// Inkbox service origin, without the `/api/v1/imessage` suffix.
    pub api_base: String,
}

impl InkboxConfig {
    /// Creates settings for an Inkbox identity on the production service.
    pub fn new(api_key: impl Into<String>, identity_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            identity_id: identity_id.into(),
            api_base: DEFAULT_API_BASE.into(),
        }
    }
}

/// Outbound iMessage provider backed by Inkbox.
pub struct Inkbox<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    config: InkboxConfig,
}

impl<'net, T, D> Inkbox<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a provider using Platform-owned HTTP resources.
    pub fn new(http_clients: ClientFactory<'net, T, D>, config: InkboxConfig) -> Self {
        Self {
            http_clients,
            config,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/api/v1/imessage/{}",
            self.config.api_base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn call<B: RequestBody>(
        &self,
        method: Method,
        url: &str,
        content_type: &str,
        body: B,
    ) -> Result<Value, ChannelError> {
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            method,
            url,
            &[
                ("Content-Type", content_type),
                ("X-API-Key", self.config.api_key.as_str()),
            ],
            body,
        )
        .await
        .map_err(|error| ChannelError::Transport {
            message: error.to_string(),
        })?;
        parse_response(response)
    }

    async fn call_json(&self, path: &str, payload: Value) -> Result<Value, ChannelError> {
        let bytes = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        self.call(
            Method::POST,
            &self.endpoint(path),
            "application/json",
            bytes.as_slice(),
        )
        .await
    }

    async fn send_text(
        &self,
        target: barracuda_imessage_gateway_plugin::MessageTarget,
        text: String,
    ) -> Result<SendReceipt, ChannelError> {
        if text.is_empty() {
            return Err(ChannelError::InvalidRequest {
                message: "message text is empty".into(),
            });
        }
        let root = self
            .call_json(
                &format!("messages?agent_identity_id={}", self.config.identity_id),
                json!({
                    "conversation_id": target.conversation_id,
                    "text": text,
                }),
            )
            .await?;
        receipt(&root)
    }

    async fn send_media_request(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> Result<SendReceipt, ChannelError> {
        let filename = request
            .filename
            .unwrap_or_else(|| default_filename(kind).into());
        let mime = request
            .mime_type
            .unwrap_or_else(|| default_mime(kind).into());
        let multipart =
            Multipart::new("barracuda-inkbox-media").file("file", filename, mime, request.body);
        let (content_type, body) =
            multipart
                .finish()
                .map_err(|error| ChannelError::InvalidRequest {
                    message: error.to_string(),
                })?;
        let body_probe = body.clone();
        let uploaded = self
            .call(Method::POST, &self.endpoint("media"), &content_type, body)
            .await?;
        if let Some(message) = body_probe.failure() {
            return Err(ChannelError::Stream(StreamError::failed(message)));
        }
        let media_url = uploaded
            .get("media_url")
            .and_then(Value::as_str)
            .ok_or_else(|| ChannelError::Platform {
                code: None,
                message: "Inkbox media response omitted media_url".into(),
            })?;
        let payload = match request.caption {
            Some(caption) => json!({
                "conversation_id": request.target.conversation_id,
                "media_urls": [media_url],
                "text": caption,
            }),
            None => json!({
                "conversation_id": request.target.conversation_id,
                "media_urls": [media_url],
            }),
        };
        let root = self
            .call_json(
                &format!("messages?agent_identity_id={}", self.config.identity_id),
                payload,
            )
            .await?;
        receipt(&root)
    }
}

impl<T, D> MessageChannel for Inkbox<'static, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect + 'static,
    D: http_client::embedded_nal_async::Dns + 'static,
{
    fn channel(&self) -> &str {
        CHANNEL
    }

    /// Sends text to the target conversation.
    ///
    /// Inkbox cannot quote a specific message, so `reply_to` is accepted and
    /// ignored rather than failing replies to inbound messages.
    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let text = match request.body {
                TextBody::Complete(text) => text,
                TextBody::Stream(mut stream) => {
                    let mut text = String::new();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk?;
                        text.push_str(chunk.as_str());
                    }
                    text
                }
            };
            self.send_text(request.target, text).await
        })
    }

    /// Uploads media and sends it to the target conversation; `reply_to` is
    /// ignored as for text.
    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move { self.send_media_request(kind, request).await })
    }

    fn react(&self, request: ReactRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            let reaction = reaction_name(&request.reaction)?;
            self.call_json(
                "reactions",
                json!({
                    "message_id": request.message_id,
                    "reaction": reaction,
                    "part_index": 0,
                }),
            )
            .await?;
            Ok(())
        })
    }

    fn set_typing(&self, request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            if request.typing {
                self.call_json(
                    "typing",
                    json!({
                        "conversation_id": request.target.conversation_id,
                    }),
                )
                .await?;
            }
            Ok(())
        })
    }
}

fn receipt(root: &Value) -> Result<SendReceipt, ChannelError> {
    let message = root.get("message").unwrap_or(root);
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ChannelError::Platform {
            code: None,
            message: "Inkbox response omitted message.id".into(),
        })?;
    Ok(SendReceipt::new(id))
}

fn parse_response(response: Response) -> Result<Value, ChannelError> {
    if response.status == 401 || response.status == 403 {
        return Err(ChannelError::Authentication);
    }
    if response.status == 429 {
        return Err(ChannelError::RateLimited);
    }
    let root: Value =
        serde_json::from_slice(&response.body).map_err(|error| ChannelError::Platform {
            code: Some(response.status.to_string()),
            message: error.to_string(),
        })?;
    if !(200..300).contains(&response.status) {
        let message = root
            .get("detail")
            .and_then(Value::as_str)
            .or_else(|| root.get("message").and_then(Value::as_str))
            .unwrap_or("Inkbox request failed");
        return Err(ChannelError::Platform {
            code: Some(response.status.to_string()),
            message: message.into(),
        });
    }
    Ok(root)
}

fn reaction_name(reaction: &str) -> Result<&'static str, ChannelError> {
    match reaction {
        "❤️" | "love" => Ok("love"),
        "👍" | "like" => Ok("like"),
        "👎" | "dislike" => Ok("dislike"),
        "😂" | "laugh" => Ok("laugh"),
        "‼️" | "emphasize" => Ok("emphasize"),
        "❓" | "question" => Ok("question"),
        "👀" | "eyes" => Ok("eyes"),
        _ => Err(ChannelError::InvalidRequest {
            message: "unsupported Inkbox tapback".into(),
        }),
    }
}

fn default_filename(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::File => "file.bin",
        MediaKind::Image => "image.jpg",
        MediaKind::Audio => "audio.bin",
        MediaKind::Video => "video.mp4",
    }
}

fn default_mime(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::File | MediaKind::Audio => "application/octet-stream",
        MediaKind::Image => "image/jpeg",
        MediaKind::Video => "video/mp4",
    }
}
