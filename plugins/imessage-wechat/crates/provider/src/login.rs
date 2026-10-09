//! iLink QR login calls used to obtain a bot token without typing it.

use alloc::{
    format,
    string::{String, ToString},
};
use core::fmt;

use barracuda_imessage_gateway_plugin::{Error, Method, Response};
use http_client::ClientFactory;
use serde_json::Value;

/// Bot type requested from iLink for a message-channel bot.
const BOT_TYPE: &str = "3";

/// One login QR code issued by iLink.
pub struct LoginQrCode {
    /// Opaque QR identifier polled with [`WechatLogin::status`].
    pub id: String,
    /// Page URL encoded into the QR code that the user scans with WeChat.
    pub url: String,
}

/// Result of one `get_qrcode_status` long poll.
pub enum LoginStatus {
    /// The QR code was not scanned yet.
    Wait,
    /// The QR code was scanned and awaits confirmation on the phone.
    Scanned,
    /// The user confirmed the login.
    Confirmed(LoginCredentials),
    /// The QR code expired.
    Expired,
}

/// Credentials returned by a confirmed login.
///
/// The type implements neither `Debug` nor `Display` so the bot token cannot
/// reach a log line by formatting.
pub struct LoginCredentials {
    /// Bot token used as the `Authorization: Bearer` credential.
    pub bot_token: String,
    /// iLink bot identifier, when reported.
    pub bot_id: Option<String>,
    /// API base URL assigned to the bot, when reported.
    pub base_url: Option<String>,
}

/// Failure of one iLink login call.
#[derive(Debug)]
pub enum LoginError {
    /// Transport, DNS, or TLS failure, or a reply that was not iLink JSON.
    Transport {
        /// Description of the failure.
        message: String,
    },
    /// iLink answered with its own error.
    Platform {
        /// Upstream error code, passed through verbatim.
        code: Option<String>,
        /// Upstream error message, passed through verbatim.
        message: String,
    },
}

impl LoginError {
    fn transport(message: impl Into<String>) -> Self {
        Self::Transport {
            message: message.into(),
        }
    }

    /// Returns the human-readable failure description.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Transport { message } | Self::Platform { message, .. } => message,
        }
    }

    /// Returns the upstream error code, when iLink reported one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Transport { .. } => None,
            Self::Platform { code, .. } => code.as_deref(),
        }
    }
}

impl fmt::Display for LoginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code() {
            Some(code) => write!(formatter, "{} (code {code})", self.message()),
            None => formatter.write_str(self.message()),
        }
    }
}

/// Unauthenticated iLink calls of the QR bot login flow.
pub struct WechatLogin<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    api_base: String,
}

impl<'net, T, D> WechatLogin<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a login client for the iLink service at `api_base`.
    pub fn new(http_clients: ClientFactory<'net, T, D>, api_base: impl Into<String>) -> Self {
        Self {
            http_clients,
            api_base: api_base.into(),
        }
    }

    /// Returns the iLink API base URL used by this client.
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Requests a new login QR code.
    pub async fn qrcode(&self) -> Result<LoginQrCode, LoginError> {
        let url = format!(
            "{}/ilink/bot/get_bot_qrcode?bot_type={BOT_TYPE}",
            self.api_base.trim_end_matches('/')
        );
        let root = self.get(&url).await?;
        let id = required_str(&root, "qrcode")?;
        let url = required_str(&root, "qrcode_img_content")?;
        Ok(LoginQrCode { id, url })
    }

    /// Long-polls the status of `qrcode`; iLink holds the request about 31 s.
    ///
    /// A poll that outlives the Gateway's send deadline reports
    /// [`LoginStatus::Wait`], as an unanswered long poll means nothing changed.
    pub async fn status(&self, qrcode: &str) -> Result<LoginStatus, LoginError> {
        let url = format!(
            "{}/ilink/bot/get_qrcode_status?qrcode={}",
            self.api_base.trim_end_matches('/'),
            QueryValue(qrcode)
        );
        let response = match self.fetch(&url).await {
            Err(Error::Timeout) => return Ok(LoginStatus::Wait),
            response => response.map_err(|error| LoginError::transport(error.to_string()))?,
        };
        let root = parse_login_response(response)?;
        let status = root
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| LoginError::transport("iLink reply has no login status"))?;
        match status {
            "wait" => Ok(LoginStatus::Wait),
            "scaned" | "scanned" => Ok(LoginStatus::Scanned),
            "expired" => Ok(LoginStatus::Expired),
            "confirmed" => Ok(LoginStatus::Confirmed(LoginCredentials {
                bot_token: required_str(&root, "bot_token")?,
                bot_id: optional_str(&root, "ilink_bot_id"),
                base_url: optional_str(&root, "baseurl"),
            })),
            other => Err(LoginError::transport(format!(
                "iLink reported an unknown login status `{other}`"
            ))),
        }
    }

    async fn get(&self, url: &str) -> Result<Value, LoginError> {
        let response = self
            .fetch(url)
            .await
            .map_err(|error| LoginError::transport(error.to_string()))?;
        parse_login_response(response)
    }

    async fn fetch(&self, url: &str) -> Result<Response, Error> {
        barracuda_imessage_gateway_plugin::send(&self.http_clients, Method::GET, url, &[], ()).await
    }
}

fn parse_login_response(response: Response) -> Result<Value, LoginError> {
    let root = serde_json::from_slice::<Value>(&response.body).ok();
    if let Some(error) = root.as_ref().and_then(platform_error) {
        return Err(error);
    }
    if !(200..300).contains(&response.status) {
        return Err(LoginError::transport(format!(
            "iLink answered HTTP {}",
            response.status
        )));
    }
    match root {
        Some(root @ Value::Object(_)) => Ok(root),
        _ => Err(LoginError::transport("iLink reply is not a JSON object")),
    }
}

fn platform_error(root: &Value) -> Option<LoginError> {
    ["ret", "errcode"].into_iter().find_map(|key| {
        let code = root.get(key).and_then(Value::as_i64).unwrap_or(0);
        (code != 0).then(|| LoginError::Platform {
            code: Some(code.to_string()),
            message: root
                .get("errmsg")
                .or_else(|| root.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("WeChat iLink API rejected the request")
                .into(),
        })
    })
}

fn required_str(root: &Value, key: &str) -> Result<String, LoginError> {
    optional_str(root, key)
        .ok_or_else(|| LoginError::transport(format!("iLink reply has no `{key}`")))
}

fn optional_str(root: &Value, key: &str) -> Option<String> {
    root.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(Into::into)
}

/// Percent-encodes one query value, keeping RFC 3986 unreserved bytes.
struct QueryValue<'a>(&'a str);

impl fmt::Display for QueryValue<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                write!(formatter, "{}", char::from(byte))?;
            } else {
                write!(formatter, "%{byte:02X}")?;
            }
        }
        Ok(())
    }
}
