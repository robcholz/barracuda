//! BlueBubbles iMessage message-channel provider.
#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
};
use core::cell::Cell;

use futures_lite::StreamExt as _;
use gateway::{
    BinaryBody, ChannelError, ChannelFuture, DeleteMessageRequest, EditMessageRequest, MediaKind,
    MessageChannel, MessageTarget, Operation, ReactRequest, SendMediaRequest, SendMessageRequest,
    SendReceipt, SetTypingRequest, TextBody,
};
use http_client::{Body, BodyError, HttpClient, Multipart, Request as HttpRequest, Response};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::{json, Value};

const URL_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// BlueBubbles server connection and streaming behavior.
pub struct BlueBubblesConfig {
    pub server_url: String,
    pub password: String,
    pub use_private_api: bool,
    /// Minimum additional UTF-8 bytes before an intermediate stream edit.
    pub stream_edit_min_delta_bytes: usize,
    /// Maximum edits after the initial streamed message, including the final edit.
    pub stream_max_edits: usize,
}

impl BlueBubblesConfig {
    pub fn new(server_url: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            server_url: server_url.into(),
            password: password.into(),
            use_private_api: true,
            stream_edit_min_delta_bytes: 128,
            stream_max_edits: 4,
        }
    }
}

/// Outbound iMessage provider backed by a BlueBubbles server.
pub struct BlueBubbles {
    http: Rc<dyn HttpClient>,
    config: BlueBubblesConfig,
    next_local_id: Cell<u64>,
}

impl BlueBubbles {
    pub fn new(http: Rc<dyn HttpClient>, config: BlueBubblesConfig) -> Self {
        Self {
            http,
            config,
            next_local_id: Cell::new(0),
        }
    }

    fn next_id(&self) -> u64 {
        let next = self.next_local_id.get().wrapping_add(1).max(1);
        self.next_local_id.set(next);
        next
    }

    fn temp_guid(&self) -> String {
        format!("temp-barracuda-{:016x}", self.next_id())
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/api/v1/{}?password={}",
            self.config.server_url.trim_end_matches('/'),
            path.trim_start_matches('/'),
            encode(&self.config.password)
        )
    }

    async fn call(&self, request: HttpRequest) -> Result<Value, ChannelError> {
        let response =
            self.http
                .execute(request)
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

    fn require_private_api(&self, operation: Operation) -> Result<(), ChannelError> {
        if self.config.use_private_api {
            Ok(())
        } else {
            Err(ChannelError::unsupported(operation))
        }
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
        if reply_to.is_some() && !self.config.use_private_api {
            return Err(ChannelError::unsupported(Operation::SendMessage));
        }
        let temp_guid = self.temp_guid();
        let mut payload = json!({
            "chatGuid": target.conversation_id,
            "tempGuid": temp_guid,
            "message": text,
            "method": self.send_method(),
        });
        if let Some(reply_to) = reply_to {
            insert_json_field(&mut payload, "selectedMessageGuid", Value::String(reply_to))?;
        }
        let root = self.call_json("message/text", payload).await?;
        receipt_from_response(&root, Some(temp_guid))
    }

    async fn send_stream(
        &self,
        target: MessageTarget,
        mut stream: gateway::TextStream,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        if !self.config.use_private_api {
            let text = collect_text(&mut stream).await?;
            return self.send_complete(target, text, reply_to).await;
        }

        let mut text = String::new();
        while text.is_empty() {
            let Some(chunk) = stream.next().await else {
                return Err(ChannelError::InvalidRequest {
                    message: "message text is empty".into(),
                });
            };
            text.push_str(&chunk?);
        }
        let receipt = self
            .send_complete(target.clone(), text.clone(), reply_to)
            .await?;
        let mut published = text.clone();
        let mut edits = 0usize;
        let max_edits = self.config.stream_max_edits.max(1);

        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk?);
            let delta = text.len().saturating_sub(published.len());
            let can_publish_intermediate = edits.saturating_add(1) < max_edits;
            if delta >= self.config.stream_edit_min_delta_bytes.max(1) && can_publish_intermediate {
                self.edit_text(&target, &receipt.message_id, text.clone())
                    .await?;
                published.clone_from(&text);
                edits = edits.saturating_add(1);
            }
        }
        if text != published {
            self.edit_text(&target, &receipt.message_id, text).await?;
        }
        Ok(receipt)
    }

    async fn edit_text(
        &self,
        _target: &MessageTarget,
        message_id: &str,
        text: String,
    ) -> Result<SendReceipt, ChannelError> {
        let root = self
            .call_json(
                &format!("message/{}/edit", encode(message_id)),
                json!({
                    "editedMessage": text,
                    "backwardsCompatibilityMessage": text,
                    "partIndex": 0,
                }),
            )
            .await?;
        receipt_from_response(&root, Some(message_id.into()))
    }

    async fn send_attachment(
        &self,
        kind: MediaKind,
        request: SendMediaRequest,
    ) -> Result<SendReceipt, ChannelError> {
        if request.reply_to.is_some() && !self.config.use_private_api {
            return Err(ChannelError::unsupported(kind.operation()));
        }
        if request.caption.is_some() && !self.config.use_private_api {
            return Err(ChannelError::InvalidRequest {
                message: "BlueBubbles attachment captions require the Private API".into(),
            });
        }
        let filename = request
            .filename
            .ok_or_else(|| ChannelError::InvalidRequest {
                message: "BlueBubbles attachments require a filename".into(),
            })?;
        let mime_type = request
            .mime_type
            .unwrap_or_else(|| default_mime(kind).into());
        let temp_guid = self.temp_guid();
        let boundary = format!("barracuda-bluebubbles-{}", self.next_id());
        let mut multipart = Multipart::new(boundary)
            .text("chatGuid", request.target.conversation_id)
            .text("tempGuid", temp_guid.clone())
            .text("name", filename.clone())
            .text("method", self.send_method())
            .text(
                "isAudioMessage",
                if kind == MediaKind::Audio {
                    "true"
                } else {
                    "false"
                },
            );
        if let Some(caption) = request.caption {
            multipart = multipart.text("subject", caption);
        }
        if let Some(reply_to) = request.reply_to {
            multipart = multipart.text("selectedMessageGuid", reply_to);
        }
        multipart = multipart.file(
            "attachment",
            filename,
            mime_type,
            map_binary_body(request.body),
        );
        let (content_type, body) =
            multipart
                .finish()
                .map_err(|error| ChannelError::InvalidRequest {
                    message: error.to_string(),
                })?;
        let root = self
            .call(
                HttpRequest::post(self.endpoint("message/attachment"))
                    .content_type(content_type)
                    .body(body),
            )
            .await?;
        receipt_from_response(&root, Some(temp_guid))
    }

    const fn send_method(&self) -> &'static str {
        if self.config.use_private_api {
            "private-api"
        } else {
            "apple-script"
        }
    }
}

impl MessageChannel for BlueBubbles {
    fn channel(&self) -> &str {
        "imessage"
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
        Box::pin(async move { self.send_attachment(kind, request).await })
    }

    fn edit_message(&self, request: EditMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            self.require_private_api(Operation::EditMessage)?;
            self.edit_text(&request.target, &request.message_id, request.text)
                .await
        })
    }

    fn delete_message(&self, request: DeleteMessageRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.require_private_api(Operation::DeleteMessage)?;
            self.call_json(
                &format!("message/{}/unsend", encode(&request.message_id)),
                json!({ "partIndex": 0 }),
            )
            .await?;
            Ok(())
        })
    }

    fn react(&self, request: ReactRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.require_private_api(Operation::React)?;
            self.call_json(
                "message/react",
                json!({
                    "chatGuid": request.target.conversation_id,
                    "selectedMessageGuid": request.message_id,
                    "reaction": map_reaction(&request.reaction)?,
                    "partIndex": 0,
                }),
            )
            .await?;
            Ok(())
        })
    }

    fn set_typing(&self, request: SetTypingRequest) -> ChannelFuture<'_, ()> {
        Box::pin(async move {
            self.require_private_api(Operation::SetTyping)?;
            let path = format!("chat/{}/typing", encode(&request.target.conversation_id));
            let http_request = if request.typing {
                HttpRequest::post(self.endpoint(&path))
            } else {
                HttpRequest::delete(self.endpoint(&path))
            };
            self.call(http_request).await?;
            Ok(())
        })
    }
}

async fn collect_text(stream: &mut gateway::TextStream) -> Result<String, ChannelError> {
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        text.push_str(&chunk?);
    }
    Ok(text)
}

fn insert_json_field(root: &mut Value, key: &str, value: Value) -> Result<(), ChannelError> {
    let Some(fields) = root.as_object_mut() else {
        return Err(ChannelError::InvalidRequest {
            message: "failed to build BlueBubbles payload".into(),
        });
    };
    fields.insert(key.into(), value);
    Ok(())
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, URL_COMPONENT).to_string()
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

const fn default_mime(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::File => "application/octet-stream",
        MediaKind::Image => "image/jpeg",
        MediaKind::Audio => "application/octet-stream",
        MediaKind::Video => "video/mp4",
    }
}

fn map_reaction(reaction: &str) -> Result<&'static str, ChannelError> {
    match reaction.trim().to_ascii_lowercase().as_str() {
        "❤️" | "❤" | "love" => Ok("love"),
        "👍" | "like" => Ok("like"),
        "👎" | "dislike" => Ok("dislike"),
        "😂" | "🤣" | "laugh" | "haha" => Ok("laugh"),
        "‼️" | "!!" | "emphasize" => Ok("emphasize"),
        "❓" | "?" | "question" => Ok("question"),
        "-❤️" | "-❤" | "-love" => Ok("-love"),
        "-👍" | "-like" => Ok("-like"),
        "-👎" | "-dislike" => Ok("-dislike"),
        "-😂" | "-🤣" | "-laugh" | "-haha" => Ok("-laugh"),
        "-‼️" | "-!!" | "-emphasize" => Ok("-emphasize"),
        "-❓" | "-?" | "-question" => Ok("-question"),
        _ => Err(ChannelError::InvalidRequest {
            message: "unsupported iMessage tapback reaction".into(),
        }),
    }
}

fn parse_response(response: Response) -> Result<Value, ChannelError> {
    if response.status == 401 || response.status == 403 {
        return Err(ChannelError::Authentication);
    }
    if response.status == 429 {
        return Err(ChannelError::RateLimited);
    }
    let root: Value = if response.body.is_empty() {
        json!({ "status": response.status })
    } else {
        serde_json::from_slice(&response.body).map_err(|error| ChannelError::Platform {
            code: Some(response.status.to_string()),
            message: error.to_string(),
        })?
    };
    let api_status = root
        .get("status")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(response.status));
    if !(200..300).contains(&response.status) || !(200..300).contains(&api_status) {
        let message = root
            .get("error")
            .and_then(|error| error.get("message").or_else(|| error.get("type")))
            .or_else(|| root.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("BlueBubbles API rejected the request");
        return Err(ChannelError::Platform {
            code: Some(api_status.to_string()),
            message: message.into(),
        });
    }
    Ok(root)
}

fn receipt_from_response(
    root: &Value,
    fallback: Option<String>,
) -> Result<SendReceipt, ChannelError> {
    root.get("data")
        .and_then(|data| data.get("guid"))
        .and_then(Value::as_str)
        .map(SendReceipt::new)
        .or_else(|| fallback.map(SendReceipt::new))
        .ok_or_else(|| ChannelError::Platform {
            code: None,
            message: "BlueBubbles response did not include a message GUID".into(),
        })
}
