//! QQ Bot API message-channel provider.
#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
};
use futures_lite::StreamExt as _;
use gateway::{
    ChannelError, ChannelFuture, MessageChannel, MessageTarget, SendMessageRequest, SendReceipt,
    TextBody,
};
use gateway_http::{Method, Response};
use http_client::ClientFactory;
use serde_json::{json, Value};

const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";

/// Credentials and endpoint for the QQ Bot API.
pub struct QQConfig {
    /// Bot application identifier sent as `X-Union-Appid`.
    pub app_id: String,
    /// QQ Bot access token.
    pub access_token: String,
    /// API origin, overridable for compatible gateways and tests.
    pub api_base: String,
}

impl QQConfig {
    /// Creates configuration for QQ's production API.
    pub fn new(app_id: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            access_token: access_token.into(),
            api_base: DEFAULT_API_BASE.into(),
        }
    }
}

/// Outbound QQ provider backed by the QQ Bot API.
pub struct QQ<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    config: QQConfig,
}

impl<'net, T, D> QQ<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a QQ channel using the supplied HTTP client factory.
    pub fn new(http_clients: ClientFactory<'net, T, D>, config: QQConfig) -> Self {
        Self {
            http_clients,
            config,
        }
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
        let (path, is_guild) = target_path(&target.conversation_id)?;
        let mut payload = if is_guild {
            json!({ "content": text })
        } else {
            json!({ "content": text, "msg_type": 0 })
        };
        if let Some(message_id) = reply_to {
            let Some(fields) = payload.as_object_mut() else {
                return Err(ChannelError::InvalidRequest {
                    message: "failed to build QQ message payload".into(),
                });
            };
            fields.insert("msg_id".into(), Value::String(message_id));
        }
        let body = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: error.to_string(),
        })?;
        let url = format!("{}{}", self.config.api_base.trim_end_matches('/'), path);
        let authorization = format!("QQBot {}", self.config.access_token);
        let response = gateway_http::send(
            &self.http_clients,
            Method::POST,
            &url,
            &[
                ("Content-Type", "application/json"),
                ("Authorization", authorization.as_str()),
                ("X-Union-Appid", self.config.app_id.as_str()),
            ],
            body.as_slice(),
        )
        .await
        .map_err(|error| ChannelError::Transport {
            message: error.to_string(),
        })?;
        parse_response(response)
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

impl<T, D> MessageChannel for QQ<'static, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect + 'static,
    D: http_client::embedded_nal_async::Dns + 'static,
{
    fn channel(&self) -> &str {
        "qq"
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

fn target_path(conversation_id: &str) -> Result<(String, bool), ChannelError> {
    if let Some(openid) = conversation_id.strip_prefix("c2c:") {
        validate_id(openid)?;
        return Ok((format!("/v2/users/{openid}/messages"), false));
    }
    if let Some(openid) = conversation_id.strip_prefix("group:") {
        validate_id(openid)?;
        return Ok((format!("/v2/groups/{openid}/messages"), false));
    }
    if let Some(channel_id) = conversation_id.strip_prefix("channel:") {
        validate_id(channel_id)?;
        return Ok((format!("/channels/{channel_id}/messages"), true));
    }
    Err(ChannelError::InvalidRequest {
        message: "QQ conversation_id must start with c2c:, group:, or channel:".into(),
    })
}

fn validate_id(id: &str) -> Result<(), ChannelError> {
    if id.is_empty() || id.contains('/') || id.contains('?') || id.contains('#') {
        return Err(ChannelError::InvalidRequest {
            message: "invalid QQ conversation identifier".into(),
        });
    }
    Ok(())
}

fn parse_response(response: Response) -> Result<SendReceipt, ChannelError> {
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
        return Err(ChannelError::Platform {
            code: root
                .get("code")
                .and_then(Value::as_i64)
                .map(|code| code.to_string())
                .or_else(|| Some(response.status.to_string())),
            message: root
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("QQ Bot API rejected the request")
                .into(),
        });
    }
    let message_id =
        root.get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ChannelError::Platform {
                code: Some(response.status.to_string()),
                message: "QQ response omitted message id".into(),
            })?;
    Ok(SendReceipt::new(message_id))
}
