//! Inkbox iMessage API message-channel provider.
#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
};

use futures_lite::StreamExt as _;
use gateway::{
    BinaryBody, ChannelError, ChannelFuture, MediaKind, MessageChannel, ReactRequest,
    SendMediaRequest, SendMessageRequest, SendReceipt, SetTypingRequest, TextBody,
};
use http_client::{Body, BodyError, HttpClient, Multipart, Request as HttpRequest, Response};
use serde_json::{json, Value};

const DEFAULT_API_BASE: &str = "https://inkbox.ai";

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
pub struct Inkbox {
    http: Rc<dyn HttpClient>,
    config: InkboxConfig,
}

impl Inkbox {
    /// Creates a provider using Barracuda's shared HTTP facade.
    pub fn new(http: Rc<dyn HttpClient>, config: InkboxConfig) -> Self {
        Self { http, config }
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/api/v1/imessage/{}",
            self.config.api_base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn call(&self, request: HttpRequest) -> Result<Value, ChannelError> {
        let response = self
            .http
            .execute(request.header("X-API-Key", &self.config.api_key))
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
            HttpRequest::post(self.endpoint(path))
                .content_type("application/json")
                .bytes(bytes),
        )
        .await
    }

    async fn send_text(
        &self,
        target: gateway::MessageTarget,
        text: String,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        if text.is_empty() {
            return Err(ChannelError::InvalidRequest {
                message: "message text is empty".into(),
            });
        }
        if reply_to.is_some() {
            return Err(ChannelError::InvalidRequest {
                message: "Inkbox iMessage does not support reply_to message IDs".into(),
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
        if request.reply_to.is_some() {
            return Err(ChannelError::InvalidRequest {
                message: "Inkbox iMessage does not support reply_to message IDs".into(),
            });
        }
        let filename = request
            .filename
            .unwrap_or_else(|| default_filename(kind).into());
        let mime = request
            .mime_type
            .unwrap_or_else(|| default_mime(kind).into());
        let multipart = Multipart::new("barracuda-inkbox-media").file(
            "file",
            filename,
            mime,
            map_binary_body(request.body),
        );
        let (content_type, body) =
            multipart
                .finish()
                .map_err(|error| ChannelError::InvalidRequest {
                    message: error.to_string(),
                })?;
        let uploaded = self
            .call(
                HttpRequest::post(self.endpoint("media"))
                    .content_type(content_type)
                    .body(body),
            )
            .await?;
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

impl MessageChannel for Inkbox {
    fn channel(&self) -> &str {
        "imessage"
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let text = match request.body {
                TextBody::Complete(text) => text,
                TextBody::Stream(mut stream) => {
                    let mut text = String::new();
                    while let Some(chunk) = stream.next().await {
                        text.push_str(&chunk?);
                    }
                    text
                }
            };
            self.send_text(request.target, text, request.reply_to).await
        })
    }

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

fn map_binary_body(body: BinaryBody) -> Body {
    match body {
        BinaryBody::Bytes(bytes) => Body::Bytes(bytes),
        BinaryBody::Stream(stream) => {
            Body::stream(Box::pin(stream.map(|chunk| {
                chunk.map_err(|error| BodyError::failed(error.to_string()))
            })))
        }
    }
}
