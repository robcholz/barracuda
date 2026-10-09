//! Receiving from a BlueBubbles server: the `new-message` webhook payload,
//! webhook registration, and the catch-up message query.
//!
//! Every response is parsed into minimal structs that keep only the fields
//! the device uses; everything else is skipped by serde without building a
//! JSON tree, and response bodies are capped before parsing.

use alloc::{format, string::String, vec::Vec};
use core::fmt;

use barracuda_imessage_gateway_plugin::{ChannelError, Method, Response};
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::Deserialize;

use crate::{encode, parse_response, BlueBubbles};

/// Webhook event the device registers for.
pub const NEW_MESSAGE_EVENT: &str = "new-message";

/// Largest webhook list the device reads; one entry is about 150 bytes.
pub const WEBHOOK_LIST_LIMIT: usize = 8 * 1024;

/// Largest catch-up page the device reads. A message without attributed
/// body or attachments, with its chat, is about 1.5 KiB of JSON.
pub const QUERY_BODY_LIMIT: usize = 16 * 1024;

/// Largest create or delete answer the device reads.
const SMALL_BODY_LIMIT: usize = 2 * 1024;

/// One message as the webhook and the message query deliver it, reduced to
/// the fields the device uses.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct InboundMessage {
    /// Message GUID, also the reply and dedup key.
    pub guid: String,
    /// Plain text; `null` for attachments and most system messages.
    #[serde(default)]
    pub text: Option<String>,
    /// Whether this Mac's account sent it.
    #[serde(rename = "isFromMe", default)]
    pub is_from_me: bool,
    /// Creation time in Unix milliseconds.
    #[serde(rename = "dateCreated", default)]
    pub date_created: Option<u64>,
    #[serde(default)]
    handle: Option<HandleRef>,
    #[serde(default, deserialize_with = "first_chat")]
    chats: Option<ChatRef>,
    /// Non-zero for group events (renames, joins).
    #[serde(rename = "itemType", default)]
    item_type: Option<i64>,
    /// Set on tapbacks and other reactions to a message.
    #[serde(
        rename = "associatedMessageType",
        default,
        deserialize_with = "not_null"
    )]
    reaction: bool,
}

impl InboundMessage {
    /// Sender address (phone number or e-mail), from `handle.address`.
    #[must_use]
    pub fn sender(&self) -> Option<&str> {
        self.handle
            .as_ref()
            .map(|handle| handle.address.as_str())
            .filter(|address| !address.is_empty())
    }

    /// GUID of the message's first chat, the send path's `chatGuid`.
    #[must_use]
    pub fn chat_guid(&self) -> Option<&str> {
        self.chats
            .as_ref()
            .map(|chat| chat.guid.as_str())
            .filter(|guid| !guid.is_empty())
    }

    /// The text of an ordinary text message: `None` for attachments,
    /// reactions, group events, and empty text.
    #[must_use]
    pub fn plain_text(&self) -> Option<&str> {
        if self.item_type.is_some_and(|item_type| item_type != 0) || self.reaction {
            return None;
        }
        // U+FFFC marks where an attachment sits in the text.
        self.text.as_deref().filter(|text| {
            !text
                .trim_matches(|character: char| {
                    character == '\u{fffc}' || character.is_whitespace()
                })
                .is_empty()
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct HandleRef {
    #[serde(default)]
    address: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct ChatRef {
    #[serde(default)]
    guid: String,
}

/// Whether a value is present and not `null`, without reading it.
fn not_null<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Option::<IgnoredAny>::deserialize(deserializer).map(|value| value.is_some())
}

/// Keeps the first chat of `chats` and skips the rest without allocating.
fn first_chat<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ChatRef>, D::Error> {
    struct First;

    impl<'de> Visitor<'de> for First {
        type Value = Option<ChatRef>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a list of chats or null")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let first = sequence.next_element::<ChatRef>()?;
            while sequence.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
    }

    deserializer.deserialize_any(First)
}

/// A webhook delivery, reduced to what the device acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookPayload {
    /// A `new-message` event.
    NewMessage(InboundMessage),
    /// Any other event type.
    Other,
}

/// The webhook body is not a BlueBubbles event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidWebhook;

impl fmt::Display for InvalidWebhook {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid BlueBubbles webhook payload")
    }
}

/// Parses a webhook body: `{"type":"new-message","data":<Message>}`.
///
/// # Errors
///
/// Returns [`InvalidWebhook`] when the body is not JSON of that shape.
pub fn parse_webhook(body: &[u8]) -> Result<WebhookPayload, InvalidWebhook> {
    #[derive(Deserialize)]
    struct Kind<'a> {
        #[serde(rename = "type", borrow)]
        kind: &'a str,
    }
    #[derive(Deserialize)]
    struct Event {
        data: InboundMessage,
    }

    let Kind { kind } = serde_json::from_slice(body).map_err(|_error| InvalidWebhook)?;
    if kind != NEW_MESSAGE_EVENT {
        return Ok(WebhookPayload::Other);
    }
    let Event { data } = serde_json::from_slice(body).map_err(|_error| InvalidWebhook)?;
    Ok(WebhookPayload::NewMessage(data))
}

/// One webhook registered on the server.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WebhookEntry {
    /// Server-assigned ID, used to delete it.
    pub id: u64,
    /// URL the server posts events to.
    #[serde(default)]
    pub url: String,
}

/// One page of the catch-up query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePage {
    /// Messages in ascending creation order.
    pub messages: Vec<InboundMessage>,
    /// Messages matching the query across every page, when reported.
    pub total: Option<u64>,
}

/// Order of a message query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    /// Oldest first.
    Ascending,
    /// Newest first.
    Descending,
}

impl Sort {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

/// A catch-up query: messages created at or after `after`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageQuery {
    /// Unix milliseconds; inclusive. `None` queries every message.
    pub after: Option<u64>,
    /// Messages skipped from the start of the result.
    pub offset: u64,
    /// Messages returned at most.
    pub limit: u32,
    /// Result order.
    pub sort: Sort,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct QueryEnvelope {
    data: Vec<InboundMessage>,
    #[serde(default)]
    metadata: Option<QueryMetadata>,
}

#[derive(Deserialize)]
struct QueryMetadata {
    #[serde(default)]
    total: Option<u64>,
}

impl<'net, T, D> BlueBubbles<'net, T, D>
where
    T: http_client::embedded_nal_async::TcpConnect,
    D: http_client::embedded_nal_async::Dns,
{
    /// Lists the server's webhooks (`GET /api/v1/webhook`).
    ///
    /// # Errors
    ///
    /// Returns the server's or the transport's failure, or
    /// [`ChannelError::Platform`] when the list exceeds
    /// [`WEBHOOK_LIST_LIMIT`].
    pub async fn list_webhooks(&self) -> Result<Vec<WebhookEntry>, ChannelError> {
        let response = self
            .call_limited(
                Method::GET,
                &self.endpoint("webhook"),
                &[],
                &[][..],
                WEBHOOK_LIST_LIMIT,
            )
            .await?;
        decode::<Envelope<Vec<WebhookEntry>>>(&response).map(|envelope| envelope.data)
    }

    /// Registers `url` for `new-message` events (`POST /api/v1/webhook`).
    /// The server keeps one entry per URL.
    ///
    /// # Errors
    ///
    /// Returns the server's or the transport's failure.
    pub async fn create_webhook(&self, url: &str) -> Result<WebhookEntry, ChannelError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "url": url,
            "events": [NEW_MESSAGE_EVENT],
        }))
        .map_err(|error| ChannelError::InvalidRequest {
            message: format!("{error}"),
        })?;
        let response = self
            .call_limited(
                Method::POST,
                &self.endpoint("webhook"),
                &[("Content-Type", "application/json")],
                body.as_slice(),
                SMALL_BODY_LIMIT,
            )
            .await?;
        decode::<Envelope<WebhookEntry>>(&response).map(|envelope| envelope.data)
    }

    /// Deletes webhook `id` (`DELETE /api/v1/webhook/:id`); an ID the
    /// server no longer has counts as deleted.
    ///
    /// # Errors
    ///
    /// Returns the server's or the transport's failure.
    pub async fn delete_webhook(&self, id: u64) -> Result<(), ChannelError> {
        let path = format!("webhook/{}", encode(&format!("{id}")));
        let response = self
            .call_limited(
                Method::DELETE,
                &self.endpoint(&path),
                &[],
                &[][..],
                SMALL_BODY_LIMIT,
            )
            .await;
        match response {
            Ok(_) => Ok(()),
            Err(ChannelError::Platform {
                code: Some(code), ..
            }) if code == "404" => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Queries messages with their chats (`POST /api/v1/message/query`).
    ///
    /// # Errors
    ///
    /// Returns the server's or the transport's failure, or
    /// [`ChannelError::Platform`] with code `body_too_large` when the page
    /// exceeds [`QUERY_BODY_LIMIT`]; ask for fewer messages then.
    pub async fn query_messages(&self, query: MessageQuery) -> Result<MessagePage, ChannelError> {
        let mut payload = serde_json::json!({
            "with": ["chat"],
            "sort": query.sort.as_str(),
            "limit": query.limit,
            "offset": query.offset,
        });
        if let (Some(after), Some(fields)) = (query.after, payload.as_object_mut()) {
            fields.insert("after".into(), after.into());
        }
        let body = serde_json::to_vec(&payload).map_err(|error| ChannelError::InvalidRequest {
            message: format!("{error}"),
        })?;
        let response = self
            .call_limited(
                Method::POST,
                &self.endpoint("message/query"),
                &[("Content-Type", "application/json")],
                body.as_slice(),
                QUERY_BODY_LIMIT,
            )
            .await?;
        let envelope = decode::<QueryEnvelope>(&response)?;
        Ok(MessagePage {
            messages: envelope.data,
            total: envelope.metadata.and_then(|metadata| metadata.total),
        })
    }

    async fn call_limited<B: barracuda_imessage_gateway_plugin::RequestBody>(
        &self,
        method: Method,
        url: &str,
        headers: &[(&str, &str)],
        body: B,
        limit: usize,
    ) -> Result<Response, ChannelError> {
        let response = barracuda_imessage_gateway_plugin::send_limited(
            &self.http_clients,
            method,
            url,
            headers,
            body,
            limit,
        )
        .await
        .map_err(|error| match error {
            barracuda_imessage_gateway_plugin::Error::BodyTooLarge => ChannelError::Platform {
                code: Some(BODY_TOO_LARGE.into()),
                message: "BlueBubbles response is too large".into(),
            },
            other => ChannelError::Transport {
                message: format!("{other}"),
            },
        })?;
        if (200..300).contains(&response.status) {
            return Ok(response);
        }
        let status = response.status;
        // The error body is small; read its message.
        Err(parse_response(response)
            .err()
            .unwrap_or(ChannelError::Platform {
                code: Some(format!("{status}")),
                message: "BlueBubbles API rejected the request".into(),
            }))
    }
}

/// [`ChannelError::Platform`] code of a response over its limit.
pub const BODY_TOO_LARGE: &str = "body_too_large";

fn decode<'a, T: Deserialize<'a>>(response: &'a Response) -> Result<T, ChannelError> {
    serde_json::from_slice(&response.body).map_err(|error| ChannelError::Platform {
        code: Some(format!("{}", response.status)),
        message: format!("unexpected BlueBubbles response: {error}"),
    })
}

/// Whether a [`ChannelError`] is a response over its limit.
#[must_use]
pub fn is_body_too_large(error: &ChannelError) -> bool {
    matches!(error, ChannelError::Platform { code: Some(code), .. } if code == BODY_TOO_LARGE)
}
