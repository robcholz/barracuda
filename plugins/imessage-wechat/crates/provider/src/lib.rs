//! WeChat iLink message-channel provider.
#![no_std]

extern crate alloc;

mod context;
mod login;
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
    ChannelError, ChannelFuture, MessageChannel, MessageTarget, SendMessageRequest, SendReceipt,
    TextBody,
};
use barracuda_imessage_gateway_plugin::{Method, Response};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;
use serde_json::{json, Value};

pub use context::{ContextTokens, MAX_CONTEXT_TOKENS};
pub use login::{LoginCredentials, LoginError, LoginQrCode, LoginStatus, WechatLogin};
pub use updates::{
    authorization, parse_oversized_updates, parse_updates, updates_body, updates_headers,
    wechat_uin, InboundText, Updates, UpdatesError, DEFAULT_LONG_POLL_MS, MAX_UPDATES_BYTES,
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
pub struct Wechat<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    config: WechatConfig,
    context_tokens: Rc<ContextTokens>,
    next_client_id: Cell<u64>,
}

impl<'net, T, D> Wechat<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    pub fn new(http_clients: ClientFactory<'net, T, D>, config: WechatConfig) -> Self {
        Self::with_context_tokens(http_clients, config, Rc::default())
    }

    /// Creates a provider whose sends attach the latest `context_token` that
    /// `context_tokens` holds for the recipient, unless the target's
    /// `thread_id` names one.
    pub fn with_context_tokens(
        http_clients: ClientFactory<'net, T, D>,
        config: WechatConfig,
        context_tokens: Rc<ContextTokens>,
    ) -> Self {
        Self {
            http_clients,
            config,
            context_tokens,
            next_client_id: Cell::new(0),
        }
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
        let url = format!(
            "{}/ilink/bot/sendmessage",
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
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            Method::POST,
            &url,
            &headers,
            bytes.as_slice(),
        )
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
        mut stream: barracuda_imessage_gateway_plugin::TextStream,
    ) -> Result<SendReceipt, ChannelError> {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk?);
        }
        self.send_text(target, text).await
    }
}

impl<T, D> MessageChannel for Wechat<'static, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect + 'static,
    D: http_client::embedded_nal_async::Dns + 'static,
{
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
