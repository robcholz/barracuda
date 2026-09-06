//! Telegram Bot API message-channel provider.
#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
};
use core::cell::Cell;

use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, DeleteMessageRequest, EditMessageRequest, MediaKind,
    MessageChannel, MessageTarget, ReactRequest, SendMediaRequest, SendMessageRequest, SendReceipt,
    SetTypingRequest, StreamError, TextBody,
};
use barracuda_imessage_gateway_plugin::{Method, Multipart, RequestBody, Response};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;
use serde_json::{json, Value};

const DEFAULT_API_BASE: &str = "https://api.telegram.org";

pub struct TelegramConfig {
    pub token: String,
    pub api_base: String,
    /// Minimum additional UTF-8 bytes before publishing another draft update.
    pub draft_min_delta_bytes: usize,
}

impl TelegramConfig {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            api_base: DEFAULT_API_BASE.into(),
            draft_min_delta_bytes: 24,
        }
    }
}

pub struct Telegram<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    config: TelegramConfig,
    next_local_id: Cell<i32>,
}

impl<'net, T, D> Telegram<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    pub fn new(http_clients: ClientFactory<'net, T, D>, config: TelegramConfig) -> Self {
        Self {
            http_clients,
            config,
            next_local_id: Cell::new(0),
        }
    }

    async fn call<B: RequestBody>(
        &self,
        method: &str,
        content_type: &str,
        body: B,
    ) -> Result<Value, ChannelError> {
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            Method::POST,
            &self.endpoint(method),
            &[("Content-Type", content_type)],
            body,
        )
        .await
        .map_err(transport_error)?;
        parse_response(response)
    }

    async fn call_json(&self, method: &str, payload: Value) -> Result<Value, ChannelError> {
        let bytes = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        self.call(method, "application/json", bytes.as_slice())
            .await
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.config.api_base.trim_end_matches('/'),
            self.config.token,
            method
        )
    }

    fn next_id(&self) -> i32 {
        let next = self.next_local_id.get().wrapping_add(1).max(1);
        self.next_local_id.set(next);
        next
    }

    async fn send_complete(
        &self,
        target: MessageTarget,
        text: String,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        if text.is_empty() {
            return Err(ChannelError::InvalidRequest {
                message: "message text is empty".into(),
            });
        }
        let mut payload = json!({
            "chat_id": chat_id(&target.conversation_id),
            "text": text,
        });
        add_reply(&mut payload, reply_to)?;
        let result = self.call_json("sendMessage", payload).await?;
        receipt_from_result(&result, None)
    }

    async fn send_stream(
        &self,
        target: MessageTarget,
        mut stream: barracuda_imessage_gateway_plugin::TextStream,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        let draft_id = self.next_id();
        let mut text = String::new();
        let mut last_draft_len = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(ChannelError::Stream)?;
            if chunk.is_empty() {
                continue;
            }
            text.push_str(chunk.as_str());
            if last_draft_len != 0
                && text.len().saturating_sub(last_draft_len)
                    < self.config.draft_min_delta_bytes.max(1)
            {
                continue;
            }
            let mut payload = json!({
                "chat_id": chat_id(&target.conversation_id),
                "draft_id": draft_id,
                "text": text,
            });
            add_reply(&mut payload, reply_to.clone())?;
            self.call_json("sendMessageDraft", payload).await?;
            last_draft_len = text.len();
        }
        self.send_complete(target, text, reply_to).await
    }

    async fn send_media_request(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> Result<SendReceipt, ChannelError> {
        let (method, field, default_name, default_mime) = match kind {
            MediaKind::File => (
                "sendDocument",
                "document",
                "file.bin",
                "application/octet-stream",
            ),
            MediaKind::Image => ("sendPhoto", "photo", "image.jpg", "image/jpeg"),
            MediaKind::Audio => (
                "sendAudio",
                "audio",
                "audio.bin",
                "application/octet-stream",
            ),
            MediaKind::Video => ("sendVideo", "video", "video.mp4", "video/mp4"),
        };
        let filename = request.filename.unwrap_or_else(|| default_name.into());
        let mime = request.mime_type.unwrap_or_else(|| default_mime.into());
        let boundary = format!("barracuda-telegram-{}", self.next_id());
        let mut multipart = Multipart::new(boundary)
            .text("chat_id", request.target.conversation_id)
            .file(field, filename, mime, request.body);
        if let Some(caption) = request.caption {
            multipart = multipart.text("caption", caption);
        }
        if let Some(reply_to) = request.reply_to {
            let message_id = parse_message_id(&reply_to)?;
            multipart = multipart.text(
                "reply_parameters",
                json!({ "message_id": message_id }).to_string(),
            );
        }
        let (content_type, body) =
            multipart
                .finish()
                .map_err(|error| ChannelError::InvalidRequest {
                    message: error.to_string(),
                })?;
        let body_probe = body.clone();
        let result = self.call(method, &content_type, body).await?;
        if let Some(message) = body_probe.failure() {
            return Err(ChannelError::Stream(StreamError::failed(message)));
        }
        receipt_from_result(&result, None)
    }
}

impl<T, D> MessageChannel for Telegram<'static, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect + 'static,
    D: http_client::embedded_nal_async::Dns + 'static,
{
    fn channel(&self) -> &str {
        "telegram"
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            match request.body {
                TextBody::Complete(text) => {
                    self.send_complete(request.target, text, request.reply_to)
                        .await
                }
                TextBody::Stream(stream) => {
                    self.send_stream(request.target, stream, request.reply_to)
                        .await
                }
            }
        })
    }

    fn send_media(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move { self.send_media_request(kind, request).await })
    }

    fn edit_message(&self, request: EditMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            let message_id = parse_message_id(&request.message_id)?;
            let result = self
                .call_json(
                    "editMessageText",
                    json!({
                        "chat_id": chat_id(&request.target.conversation_id),
                        "message_id": message_id,
                        "text": request.text,
                    }),
                )
                .await?;
            receipt_from_result(&result, Some(request.message_id))
        })
    }

    fn delete_message(&self, request: DeleteMessageRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.call_json(
                "deleteMessage",
                json!({
                    "chat_id": chat_id(&request.target.conversation_id),
                    "message_id": parse_message_id(&request.message_id)?,
                }),
            )
            .await?;
            Ok(())
        })
    }

    fn react(&self, request: ReactRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            let reaction = if request.reaction.is_empty() {
                Value::Array(alloc::vec::Vec::new())
            } else {
                json!([{ "type": "emoji", "emoji": request.reaction }])
            };
            self.call_json(
                "setMessageReaction",
                json!({
                    "chat_id": chat_id(&request.target.conversation_id),
                    "message_id": parse_message_id(&request.message_id)?,
                    "reaction": reaction,
                }),
            )
            .await?;
            Ok(())
        })
    }

    fn set_typing(&self, request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            if !request.typing {
                return Ok(());
            }
            self.call_json(
                "sendChatAction",
                json!({
                    "chat_id": chat_id(&request.target.conversation_id),
                    "action": "typing",
                }),
            )
            .await?;
            Ok(())
        })
    }
}

fn chat_id(value: &str) -> Value {
    value
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(value.into()))
}

fn parse_message_id(value: &str) -> Result<i64, ChannelError> {
    value
        .parse::<i64>()
        .map_err(|_| ChannelError::InvalidRequest {
            message: "Telegram message_id must be an integer".into(),
        })
}

fn add_reply(payload: &mut Value, reply_to: Option<String>) -> Result<(), ChannelError> {
    if let Some(reply_to) = reply_to {
        payload["reply_parameters"] = json!({
            "message_id": parse_message_id(&reply_to)?,
        });
    }
    Ok(())
}

fn receipt_from_result(
    result: &Value,
    fallback: Option<String>,
) -> Result<SendReceipt, ChannelError> {
    let message_id = result
        .get("message_id")
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
        .or(fallback)
        .ok_or_else(|| ChannelError::Platform {
            code: None,
            message: "Telegram response did not include message_id".into(),
        })?;
    Ok(SendReceipt::new(message_id))
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
    if !(200..300).contains(&response.status) || root.get("ok") != Some(&Value::Bool(true)) {
        return Err(ChannelError::Platform {
            code: root
                .get("error_code")
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .or_else(|| Some(response.status.to_string())),
            message: root
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Telegram API rejected the request")
                .into(),
        });
    }
    root.get("result")
        .cloned()
        .ok_or_else(|| ChannelError::Platform {
            code: None,
            message: "Telegram response did not include result".into(),
        })
}

fn transport_error(error: barracuda_imessage_gateway_plugin::Error) -> ChannelError {
    ChannelError::Transport {
        message: error.to_string(),
    }
}
