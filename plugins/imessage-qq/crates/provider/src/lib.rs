//! QQ Bot API message-channel provider.
#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, MessageChannel, MessageTarget, SendMessageRequest, SendReceipt,
    TextBody,
};
use barracuda_imessage_gateway_plugin::{Method, Response};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;
use serde_json::{json, Value};

/// Default QQ Bot OpenAPI origin.
pub const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";
/// Default endpoint exchanging an App Secret for an access token.
pub const DEFAULT_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
/// A cached access token is replaced once it has this little lifetime left.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Credentials and endpoints for the QQ Bot API.
pub struct QQConfig {
    /// Bot application identifier sent as `appId` and `X-Union-Appid`.
    pub app_id: String,
    /// Bot App Secret exchanged for short-lived access tokens.
    pub app_secret: String,
    /// API origin, overridable for compatible gateways and tests.
    pub api_base: String,
    /// Access-token endpoint, overridable for compatible gateways and tests.
    pub token_url: String,
}

impl QQConfig {
    /// Creates configuration for QQ's production API and token endpoint.
    pub fn new(app_id: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            api_base: DEFAULT_API_BASE.into(),
            token_url: DEFAULT_TOKEN_URL.into(),
        }
    }
}

/// Failure while exchanging the App Secret for an access token.
///
/// Neither variant carries the App Secret or a token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenError {
    /// QQ answered and refused to issue a token for these credentials.
    Rejected {
        /// QQ's own error code, verbatim, when it sent one.
        code: Option<String>,
        /// QQ's own error message, verbatim, when it sent one.
        message: Option<String>,
    },
    /// The token endpoint was unreachable or did not answer with a usable JSON reply.
    Unavailable {
        /// Description of the transport or response failure.
        message: String,
    },
}

impl core::fmt::Display for TokenError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rejected { code, message } => write!(
                formatter,
                "QQ rejected the App Secret (code {}): {}",
                code.as_deref().unwrap_or("none"),
                message.as_deref().unwrap_or("no message")
            ),
            Self::Unavailable { message } => {
                write!(formatter, "QQ token endpoint unavailable: {message}")
            }
        }
    }
}

impl core::error::Error for TokenError {}

/// Access token held in RAM only, with the instant QQ stops accepting it.
struct AccessToken {
    value: String,
    expires_at: Instant,
}

impl AccessToken {
    fn is_fresh(&self, now: Instant) -> bool {
        self.expires_at
            .checked_duration_since(now)
            .is_some_and(|remaining| remaining > TOKEN_REFRESH_MARGIN)
    }
}

/// Outbound QQ provider backed by the QQ Bot API.
///
/// The provider exchanges the configured App Secret for an access token on
/// demand and keeps that token in RAM only. Concurrent sends share one
/// refresh.
pub struct QQ<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    config: QQConfig,
    access_token: Mutex<NoopRawMutex, Option<AccessToken>>,
}

impl<'net, T, D> QQ<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a QQ channel using the supplied HTTP client factory.
    ///
    /// No request is made until [`Self::authenticate`] or the first send.
    pub fn new(http_clients: ClientFactory<'net, T, D>, config: QQConfig) -> Self {
        Self {
            http_clients,
            config,
            access_token: Mutex::new(None),
        }
    }

    /// Exchanges the App Secret for a fresh access token and keeps it for later sends.
    pub async fn authenticate(&self) -> Result<(), TokenError> {
        let mut cached = self.access_token.lock().await;
        cached.replace(self.fetch_access_token().await?);
        Ok(())
    }

    /// Returns a usable access token, refreshing it when it is missing, near
    /// expiry, or equal to `rejected` (a token QQ just answered 401 to).
    async fn access_token(&self, rejected: Option<&str>) -> Result<String, TokenError> {
        let mut cached = self.access_token.lock().await;
        if let Some(token) = cached.as_ref() {
            if rejected == Some(token.value.as_str()) {
                cached.take();
            } else if token.is_fresh(Instant::now()) {
                return Ok(token.value.clone());
            }
        }
        let token = self.fetch_access_token().await?;
        let value = token.value.clone();
        cached.replace(token);
        Ok(value)
    }

    async fn fetch_access_token(&self) -> Result<AccessToken, TokenError> {
        let body = serde_json::to_vec(&json!({
            "appId": self.config.app_id,
            "clientSecret": self.config.app_secret,
        }))
        .map_err(|error| TokenError::Unavailable {
            message: error.to_string(),
        })?;
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            Method::POST,
            &self.config.token_url,
            &[("Content-Type", "application/json")],
            body.as_slice(),
        )
        .await
        .map_err(|error| TokenError::Unavailable {
            message: error.to_string(),
        })?;
        parse_token_response(&response, Instant::now())
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
        let token = self.access_token(None).await.map_err(token_channel_error)?;
        let mut response = self.post_message(&url, &token, body.as_slice()).await?;
        if response.status == 401 {
            let token = self
                .access_token(Some(token.as_str()))
                .await
                .map_err(token_channel_error)?;
            response = self.post_message(&url, &token, body.as_slice()).await?;
        }
        parse_response(response)
    }

    async fn post_message(
        &self,
        url: &str,
        access_token: &str,
        body: &[u8],
    ) -> Result<Response, ChannelError> {
        let authorization = format!("QQBot {access_token}");
        barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            Method::POST,
            url,
            &[
                ("Content-Type", "application/json"),
                ("Authorization", authorization.as_str()),
                ("X-Union-Appid", self.config.app_id.as_str()),
            ],
            body,
        )
        .await
        .map_err(|error| ChannelError::Transport {
            message: error.to_string(),
        })
    }

    async fn send_stream(
        &self,
        target: MessageTarget,
        mut stream: barracuda_imessage_gateway_plugin::TextStream,
        reply_to: Option<String>,
    ) -> Result<SendReceipt, ChannelError> {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            text.push_str(chunk.as_str());
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

fn parse_token_response(response: &Response, now: Instant) -> Result<AccessToken, TokenError> {
    let Ok(root) = serde_json::from_slice::<Value>(&response.body) else {
        return Err(TokenError::Unavailable {
            message: format!(
                "QQ token endpoint answered HTTP {} without JSON",
                response.status
            ),
        });
    };
    if response.status >= 500 {
        return Err(TokenError::Unavailable {
            message: format!("QQ token endpoint answered HTTP {}", response.status),
        });
    }
    let access_token = root
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    let Some(access_token) = access_token.filter(|_| (200..300).contains(&response.status)) else {
        return Err(TokenError::Rejected {
            code: root.get("code").and_then(upstream_code),
            message: root
                .get("message")
                .and_then(Value::as_str)
                .map(String::from),
        });
    };
    let expires_in = root
        .get("expires_in")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .ok_or_else(|| TokenError::Unavailable {
            message: "QQ token response omitted a valid expires_in".into(),
        })?;
    let expires_at = Duration::try_from_secs(expires_in)
        .map_or(Instant::MAX, |lifetime| now.saturating_add(lifetime));
    Ok(AccessToken {
        value: access_token.into(),
        expires_at,
    })
}

fn upstream_code(code: &Value) -> Option<String> {
    match code {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    }
}

fn token_channel_error(error: TokenError) -> ChannelError {
    match error {
        TokenError::Rejected { code, message } => ChannelError::Platform {
            code,
            message: message.unwrap_or_else(|| "QQ rejected the App Secret".into()),
        },
        TokenError::Unavailable { message } => ChannelError::Transport { message },
    }
}
