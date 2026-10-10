//! QQ scan-to-bind: the `q.qq.com` calls that hand over a bot's App ID and
//! App Secret once its owner scans a QR code, so neither is typed.
//!
//! The caller makes a random 32-byte [`BindKey`] and creates a task with it;
//! the QR code encodes [`connect_url`] for that task. QQ seals the App Secret
//! with the key (AES-256-GCM), so only the device that made the key can open
//! it. A task that nobody has completed reports [`BindStatus::Waiting`]: QQ
//! does not report the scan itself.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use barracuda_imessage_gateway_plugin::{Method, Response};
use barracuda_tls::{aes_256_gcm_open, GCM_NONCE_LEN};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_client::ClientFactory;
use serde_json::{json, Value};

/// Origin of the QQ bot portal serving the bind-task calls.
pub const DEFAULT_PORTAL_BASE: &str = "https://q.qq.com";
/// Page the QR code opens in mobile QQ; QQ serves it from the production
/// portal whichever portal created the task.
const CONNECT_PAGE: &str = "https://q.qq.com/qqbot/openclaw/connect.html";
/// Names this device to QQ in the connect page's `source` parameter.
const SOURCE: &str = "barracuda";
const CREATE_PATH: &str = "/lite/create_bind_task";
const POLL_PATH: &str = "/lite/poll_bind_result";
/// `q.qq.com` answers a request without this header with a script challenge.
const HEADERS: &[(&str, &str)] = &[
    ("Content-Type", "application/json"),
    ("Accept", "application/json"),
];

/// The AES-256 key QQ seals the App Secret with. Make it from the Platform
/// entropy, one per task.
pub struct BindKey([u8; 32]);

impl BindKey {
    /// Wraps 32 random bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn encoded(&self) -> String {
        STANDARD.encode(self.0)
    }
}

/// Result of one `poll_bind_result` call.
pub enum BindStatus {
    /// Nobody has completed the binding on a phone yet.
    Waiting,
    /// The task ended without a binding.
    Expired,
    /// The binding was completed; the App Secret is already opened.
    Completed(BoundBot),
}

/// The bot a completed binding names.
///
/// The type implements neither `Debug` nor `Display` so the App Secret cannot
/// reach a log line by formatting.
pub struct BoundBot {
    /// Bot application identifier.
    pub app_id: String,
    /// Bot App Secret, opened with the task's [`BindKey`].
    pub app_secret: String,
    /// The `user_openid` of the person who completed the binding, when QQ
    /// reported one: the sender id of their direct messages to the bot.
    pub user_openid: Option<String>,
}

/// Failure of one bind call.
#[derive(Debug)]
pub enum BindError {
    /// Transport, DNS, or TLS failure, or a reply that was not portal JSON.
    Transport {
        /// Description of the failure.
        message: String,
    },
    /// The portal answered with its own error.
    Platform {
        /// Upstream `retcode`, passed through verbatim.
        code: Option<String>,
        /// Upstream `msg`, passed through verbatim.
        message: String,
    },
    /// The sealed App Secret did not open with the task's key.
    Secret,
}

impl BindError {
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
            Self::Secret => "the QQ App Secret did not open with this device's key",
        }
    }

    /// Returns the upstream error code, when the portal reported one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Platform { code, .. } => code.as_deref(),
            Self::Transport { .. } | Self::Secret => None,
        }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code() {
            Some(code) => write!(formatter, "{} (code {code})", self.message()),
            None => formatter.write_str(self.message()),
        }
    }
}

/// Unauthenticated `q.qq.com` calls of the scan-to-bind flow.
pub struct QQBind<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    portal_base: String,
}

impl<'net, T, D> QQBind<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a bind client for the portal at `portal_base`.
    pub fn new(http_clients: ClientFactory<'net, T, D>, portal_base: impl Into<String>) -> Self {
        Self {
            http_clients,
            portal_base: portal_base.into(),
        }
    }

    /// Creates a bind task sealed to `key` and returns its id.
    pub async fn create(&self, key: &BindKey) -> Result<String, BindError> {
        let data = self
            .call(CREATE_PATH, &json!({ "key": key.encoded() }))
            .await?;
        data.get("task_id")
            .and_then(Value::as_str)
            .filter(|task| !task.is_empty())
            .map(Into::into)
            .ok_or_else(|| BindError::transport("QQ bind task reply has no task_id"))
    }

    /// Polls task `task_id`, opening the App Secret with `key` once the
    /// binding is completed.
    pub async fn poll(&self, task_id: &str, key: &BindKey) -> Result<BindStatus, BindError> {
        let data = self.call(POLL_PATH, &json!({ "task_id": task_id })).await?;
        match data.get("status").and_then(Value::as_u64).unwrap_or(0) {
            0 | 1 => Ok(BindStatus::Waiting),
            2 => completed(&data, key).map(BindStatus::Completed),
            3 => Ok(BindStatus::Expired),
            other => Err(BindError::transport(format!(
                "QQ reported an unknown bind status {other}"
            ))),
        }
    }

    async fn call(&self, path: &str, body: &Value) -> Result<Value, BindError> {
        let url = format!("{}{path}", self.portal_base.trim_end_matches('/'));
        let body =
            serde_json::to_vec(body).map_err(|error| BindError::transport(error.to_string()))?;
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            Method::POST,
            &url,
            HEADERS,
            body.as_slice(),
        )
        .await
        .map_err(|error| BindError::transport(error.to_string()))?;
        parse_reply(&response)
    }
}

/// The URL a QR code for task `task_id` encodes.
#[must_use]
pub fn connect_url(task_id: &str) -> String {
    format!(
        "{CONNECT_PAGE}?task_id={}&source={SOURCE}&_wv=2",
        QueryValue(task_id)
    )
}

/// Returns `data` of a `{"retcode":0,"data":{…}}` reply.
fn parse_reply(response: &Response) -> Result<Value, BindError> {
    let root = serde_json::from_slice::<Value>(&response.body).ok();
    if !(200..300).contains(&response.status) {
        return Err(BindError::transport(format!(
            "QQ portal answered HTTP {}",
            response.status
        )));
    }
    let Some(mut root) = root.filter(Value::is_object) else {
        return Err(BindError::transport("QQ portal reply is not a JSON object"));
    };
    match root.get("retcode").and_then(Value::as_i64) {
        Some(0) => {}
        code => {
            return Err(BindError::Platform {
                code: code.map(|code| code.to_string()),
                message: root
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("QQ portal rejected the request")
                    .into(),
            })
        }
    }
    match root.get_mut("data").map(Value::take) {
        Some(data @ Value::Object(_)) => Ok(data),
        _ => Err(BindError::transport("QQ portal reply has no data")),
    }
}

fn completed(data: &Value, key: &BindKey) -> Result<BoundBot, BindError> {
    let app_id = match data.get("bot_appid") {
        Some(Value::String(id)) => Some(id.clone()),
        Some(Value::Number(id)) => Some(id.to_string()),
        _ => None,
    }
    .filter(|id| !id.is_empty() && id != "0")
    .ok_or_else(|| BindError::transport("completed QQ binding has no bot_appid"))?;
    let sealed = data
        .get("bot_encrypt_secret")
        .and_then(Value::as_str)
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| BindError::transport("completed QQ binding has no bot_encrypt_secret"))?;
    let app_secret = open_secret(sealed, key)?;
    let user_openid = data
        .get("user_openid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(Into::into);
    Ok(BoundBot {
        app_id,
        app_secret,
        user_openid,
    })
}

/// Opens base64 `nonce(12) ‖ ciphertext ‖ tag(16)` with `key`.
fn open_secret(sealed: &str, key: &BindKey) -> Result<String, BindError> {
    let bytes: Vec<u8> = STANDARD
        .decode(sealed)
        .map_err(|_error| BindError::Secret)?;
    let (nonce, rest) = bytes
        .split_first_chunk::<GCM_NONCE_LEN>()
        .ok_or(BindError::Secret)?;
    let plaintext = aes_256_gcm_open(&key.0, nonce, rest).map_err(|_error| BindError::Secret)?;
    String::from_utf8(plaintext).map_err(|_error| BindError::Secret)
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
