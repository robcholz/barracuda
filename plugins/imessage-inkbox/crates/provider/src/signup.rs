//! Inkbox agent self-signup and identity lookup.
//!
//! These calls follow the Inkbox SDK's `Inkbox.signup`, `Inkbox.verify_signup`,
//! `Inkbox.resend_signup_verification`, and `IdentitiesResource.get`.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use barracuda_imessage_gateway_plugin::Method;
use http_client::ClientFactory;
use serde_json::{json, Map, Value};

/// Fields of one agent self-signup request.
pub struct SignupRequest<'a> {
    /// Email address of the person who approves the agent.
    pub human_email: &'a str,
    /// Message included in the verification email.
    pub note_to_human: &'a str,
    /// Optional human-readable agent name.
    pub display_name: Option<&'a str>,
    /// Optional identifier of the calling runtime.
    pub harness: Option<&'a str>,
}

/// Agent account returned by a successful signup.
///
/// `api_key` is shown by Inkbox only once. This type has no `Debug`
/// implementation so the key cannot reach a log through formatting.
pub struct SignupAccount {
    /// Agent-scoped Inkbox API key.
    pub api_key: String,
    /// Handle of the agent identity created by signup.
    pub agent_handle: String,
    /// Mailbox address provisioned for the agent.
    pub email_address: String,
    /// Claim state of the new agent, such as `agent_unclaimed`.
    pub claim_status: String,
}

/// Failure of one signup, identity, verification, or resend call.
#[derive(Debug)]
pub enum SignupError {
    /// Inkbox answered with a 4xx status.
    Rejected {
        /// Upstream HTTP status.
        status: u16,
        /// Upstream machine-readable code, when Inkbox sent one.
        code: Option<String>,
        /// Upstream human-readable message, when Inkbox sent one.
        message: Option<String>,
    },
    /// Transport failure, 5xx status, non-JSON reply, or an incomplete success.
    Unavailable {
        /// Upstream machine-readable code, when Inkbox sent one.
        code: Option<String>,
        /// Upstream or transport message.
        message: Option<String>,
    },
}

impl fmt::Display for SignupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected { status, .. } => {
                write!(formatter, "Inkbox rejected the request with HTTP {status}")
            }
            Self::Unavailable { .. } => formatter.write_str("Inkbox is unavailable"),
        }
    }
}

/// Client for the unauthenticated agent self-signup flow.
pub struct InkboxSignup<'net, T = http_client::Tcp, D = http_client::Resolver> {
    http_clients: ClientFactory<'net, T, D>,
    api_base: String,
}

impl<'net, T, D> InkboxSignup<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Creates a client for the Inkbox service origin `api_base`, without the `/api` suffix.
    pub fn new(http_clients: ClientFactory<'net, T, D>, api_base: impl Into<String>) -> Self {
        Self {
            http_clients,
            api_base: api_base.into(),
        }
    }

    /// Creates an agent and sends the verification email to `human_email`.
    pub async fn sign_up(&self, request: &SignupRequest<'_>) -> Result<SignupAccount, SignupError> {
        let mut body = Map::new();
        body.insert("human_email".into(), request.human_email.into());
        body.insert("note_to_human".into(), request.note_to_human.into());
        if let Some(display_name) = request.display_name {
            body.insert("display_name".into(), display_name.into());
        }
        if let Some(harness) = request.harness {
            body.insert("harness".into(), harness.into());
        }
        let root = self
            .call(
                Method::POST,
                "agent-signup",
                None,
                Some(Value::Object(body)),
            )
            .await?;
        Ok(SignupAccount {
            api_key: required_string(&root, "api_key")?,
            agent_handle: required_string(&root, "agent_handle")?,
            email_address: required_string(&root, "email_address")?,
            claim_status: required_string(&root, "claim_status")?,
        })
    }

    /// Returns the identity UUID of `agent_handle`, as used by `agent_identity_id=`.
    pub async fn resolve_identity(
        &self,
        api_key: &str,
        agent_handle: &str,
    ) -> Result<String, SignupError> {
        let path = format!("identities/{}", PathSegment(agent_handle));
        let root = self.call(Method::GET, &path, Some(api_key), None).await?;
        required_string(&root, "id")
    }

    /// Submits the emailed verification code and returns the new claim status.
    pub async fn verify(&self, api_key: &str, code: &str) -> Result<String, SignupError> {
        let root = self
            .call(
                Method::POST,
                "agent-signup/verify",
                Some(api_key),
                Some(json!({ "verification_code": code })),
            )
            .await?;
        required_string(&root, "claim_status")
    }

    /// Asks Inkbox to send the verification email again.
    pub async fn resend_verification(&self, api_key: &str) -> Result<(), SignupError> {
        self.call(
            Method::POST,
            "agent-signup/resend-verification",
            Some(api_key),
            None,
        )
        .await?;
        Ok(())
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        api_key: Option<&str>,
        payload: Option<Value>,
    ) -> Result<Value, SignupError> {
        let url = format!("{}/api/v1/{path}", self.api_base.trim_end_matches('/'));
        let body = match payload {
            Some(payload) => {
                serde_json::to_vec(&payload).map_err(|error| SignupError::Unavailable {
                    code: None,
                    message: Some(error.to_string()),
                })?
            }
            None => Vec::new(),
        };
        let mut headers = Vec::with_capacity(3);
        headers.push(("Accept", "application/json"));
        if !body.is_empty() {
            headers.push(("Content-Type", "application/json"));
        }
        if let Some(api_key) = api_key {
            headers.push(("X-API-Key", api_key));
        }
        let response = barracuda_imessage_gateway_plugin::send(
            &self.http_clients,
            method,
            &url,
            &headers,
            body.as_slice(),
        )
        .await
        .map_err(|error| SignupError::Unavailable {
            code: None,
            message: Some(error.to_string()),
        })?;
        let Ok(root) = serde_json::from_slice::<Value>(&response.body) else {
            return Err(SignupError::Unavailable {
                code: None,
                message: Some(format!(
                    "Inkbox returned a non-JSON reply with HTTP {}",
                    response.status
                )),
            });
        };
        if (200..300).contains(&response.status) {
            return Ok(root);
        }
        let (code, message) = upstream_detail(&root);
        if (400..500).contains(&response.status) {
            Err(SignupError::Rejected {
                status: response.status,
                code,
                message,
            })
        } else {
            Err(SignupError::Unavailable { code, message })
        }
    }
}

fn required_string(root: &Value, field: &str) -> Result<String, SignupError> {
    root.get(field)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| SignupError::Unavailable {
            code: None,
            message: Some(format!("Inkbox response omitted {field}")),
        })
}

/// Extracts Inkbox's own error code and message.
///
/// Inkbox reports errors as `{"detail": "<message>"}`,
/// `{"detail": {"code", "message"}}`, or FastAPI validation errors
/// `{"detail": [{"msg", ...}]}`.
fn upstream_detail(root: &Value) -> (Option<String>, Option<String>) {
    match root.get("detail") {
        Some(Value::String(message)) => (None, Some(message.clone())),
        Some(Value::Object(detail)) => (
            detail.get("code").and_then(code_text),
            detail
                .get("message")
                .and_then(Value::as_str)
                .map(String::from),
        ),
        Some(Value::Array(items)) => (
            None,
            items
                .first()
                .and_then(|item| item.get("msg"))
                .and_then(Value::as_str)
                .map(String::from),
        ),
        _ => (
            root.get("code").and_then(code_text),
            root.get("message")
                .and_then(Value::as_str)
                .map(String::from),
        ),
    }
}

fn code_text(code: &Value) -> Option<String> {
    match code {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    }
}

/// Percent-encodes one URL path segment, keeping RFC 3986 unreserved bytes.
struct PathSegment<'a>(&'a str);

impl fmt::Display for PathSegment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                fmt::Write::write_char(formatter, char::from(byte))?;
            } else {
                write!(formatter, "%{byte:02X}")?;
            }
        }
        Ok(())
    }
}
