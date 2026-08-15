//! WeChat iLink message-channel provider.
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
    ChannelError, ChannelFuture, MessageChannel, MessageTarget, SendMessageRequest, SendReceipt,
    TextBody,
};
use http_client::{HttpClient, Request as HttpRequest, Response};
use serde_json::{json, Value};

const DEFAULT_API_BASE: &str = "https://ilinkai.weixin.qq.com";
const MAX_TEXT_BYTES: usize = 4_000;

/// Connection settings matching the headers used by the WeChat iLink bot API.
pub struct WechatConfig {
    pub token: String,
    pub api_base: String,
    pub app_id: String,
    pub client_version: String,
    pub route_tag: Option<String>,
    /// Base64-encoded decimal value used by the `X-WECHAT-UIN` header.
    pub x_wechat_uin: String,
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
        }
    }
}

/// Outbound WeChat provider backed by the iLink bot API.
pub struct Wechat {
    http: Rc<dyn HttpClient>,
    config: WechatConfig,
    next_client_id: Cell<u64>,
}

impl Wechat {
    pub fn new(http: Rc<dyn HttpClient>, config: WechatConfig) -> Self {
        Self {
            http,
            config,
            next_client_id: Cell::new(0),
        }
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
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        if text.is_empty() {
            return Err(ChannelError::InvalidRequest {
                message: "message text is empty".into(),
            });
        }
        if reply_to.is_some() {
            return Err(ChannelError::InvalidRequest {
                message: "WeChat iLink does not support reply_to message IDs".into(),
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
        let client_id = self.next_client_id();
        let mut msg = json!({
            "from_user_id": "",
            "to_user_id": target.conversation_id,
            "client_id": client_id,
            "message_type": 2,
            "message_state": 2,
            "item_list": [{
                "type": 1,
                "text_item": { "text": text },
            }],
        });
        if let Some(context_token) = &target.thread_id {
            let Some(fields) = msg.as_object_mut() else {
                return Err(ChannelError::InvalidRequest {
                    message: "failed to build WeChat message payload".into(),
                });
            };
            fields.insert("context_token".into(), Value::String(context_token.clone()));
        }
        let payload = json!({
            "msg": msg,
            "base_info": { "channel_version": "barracuda-wechat" },
        });
        let bytes = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        let mut request = HttpRequest::post(format!(
            "{}/ilink/bot/sendmessage",
            self.config.api_base.trim_end_matches('/')
        ))
        .content_type("application/json")
        .header("iLink-App-Id", self.config.app_id.clone())
        .header(
            "iLink-App-ClientVersion",
            self.config.client_version.clone(),
        )
        .header("X-WECHAT-UIN", self.config.x_wechat_uin.clone())
        .header("AuthorizationType", "ilink_bot_token")
        .header("Authorization", format!("Bearer {}", self.config.token))
        .bytes(bytes);
        if let Some(route_tag) = &self.config.route_tag {
            request = request.header("SKRouteTag", route_tag.clone());
        }
        let response =
            self.http
                .execute(request)
                .await
                .map_err(|error| ChannelError::Transport {
                    message: error.to_string(),
                })?;
        parse_response(response)?;
        Ok(SendReceipt::new(client_id))
    }

    async fn send_stream(
        &self,
        target: MessageTarget,
        mut stream: gateway::TextStream,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk?);
        }
        self.send_text(target, text, reply_to).await
    }
}

impl MessageChannel for Wechat {
    fn channel(&self) -> &str {
        "wechat"
    }

    fn send_message(&self, request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async move {
            match request.body {
                TextBody::Complete(text) => {
                    self.send_text(request.target, text, request.reply_to).await
                }
                TextBody::Stream(stream) => {
                    self.send_stream(request.target, stream, request.reply_to)
                        .await
                }
            }
        })
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

fn parse_response(response: Response) -> Result<(), ChannelError> {
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
        return Ok(());
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
    Ok(())
}
