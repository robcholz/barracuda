//! QQ Bot WebSocket gateway protocol: payloads, opcodes, and close codes.
//!
//! See <https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/interface-framework/event-emit.html>.
//! Every gateway payload is `{"op":n,"s":seq?,"t":"EVENT"?,"d":…}`. Payloads
//! are parsed in two passes, first the envelope and then only the `d` this
//! client uses, into small structs; no JSON tree is built.

use alloc::string::String;

use serde::{Deserialize, Serialize};

/// `GROUP_AND_C2C_EVENT`: direct messages and group messages that mention
/// the bot.
pub const INTENTS: u32 = 1 << 25;

/// Gateway opcodes.
pub mod op {
    /// An event (`t` names it), with sequence number `s`.
    pub const DISPATCH: u8 = 0;
    /// Client heartbeat carrying the last `s`.
    pub const HEARTBEAT: u8 = 1;
    /// Client login.
    pub const IDENTIFY: u8 = 2;
    /// Client session resumption.
    pub const RESUME: u8 = 6;
    /// The server asks the client to reconnect and resume.
    pub const RECONNECT: u8 = 7;
    /// The session cannot be used; `d` says whether it can be resumed.
    pub const INVALID_SESSION: u8 = 9;
    /// First server payload, with the heartbeat interval.
    pub const HELLO: u8 = 10;
    /// Server acknowledgement of a heartbeat.
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Event names this client handles.
pub mod event {
    /// Identify succeeded; `d.session_id` names the session.
    pub const READY: &str = "READY";
    /// Resume succeeded and missed events were replayed.
    pub const RESUMED: &str = "RESUMED";
    /// A direct message to the bot.
    pub const C2C_MESSAGE_CREATE: &str = "C2C_MESSAGE_CREATE";
    /// A group message that mentions the bot.
    pub const GROUP_AT_MESSAGE_CREATE: &str = "GROUP_AT_MESSAGE_CREATE";
}

/// The envelope of every gateway payload, without `d`.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    /// Opcode.
    pub op: u8,
    /// Sequence number of a dispatch.
    #[serde(default)]
    pub s: Option<u64>,
    /// Event name of a dispatch.
    #[serde(default)]
    pub t: Option<String>,
}

#[derive(Deserialize)]
struct Data<T> {
    d: T,
}

/// Parses the envelope of a gateway payload.
///
/// # Errors
///
/// Fails when `text` is not a gateway payload.
pub fn parse_envelope(text: &str) -> Result<Envelope, serde_json::Error> {
    serde_json::from_str(text)
}

/// Parses the `d` of a gateway payload as `T`.
///
/// # Errors
///
/// Fails when `d` does not have the shape of `T`.
pub fn parse_data<'a, T: Deserialize<'a>>(text: &'a str) -> Result<T, serde_json::Error> {
    serde_json::from_str::<Data<T>>(text).map(|data| data.d)
}

/// `d` of Hello.
#[derive(Debug, Deserialize)]
pub struct Hello {
    /// Milliseconds between heartbeats.
    pub heartbeat_interval: u64,
}

/// `d` of READY.
#[derive(Debug, Deserialize)]
pub struct Ready {
    /// Session to resume after a drop.
    pub session_id: String,
}

/// `d` of a message event. Only what the receive loop uses is parsed.
#[derive(Debug, Deserialize)]
pub struct MessageEvent {
    /// Message id; the passive reply's `msg_id`.
    pub id: String,
    /// Text content; empty for attachments and cards.
    #[serde(default)]
    pub content: String,
    /// The sender.
    #[serde(default)]
    pub author: Author,
    /// Group of a group message.
    #[serde(default)]
    pub group_openid: Option<String>,
    /// 0 for plain text; other values are cards, quotes, and forwards.
    #[serde(default)]
    pub message_type: Option<u32>,
}

/// The sender of a message event.
#[derive(Debug, Default, Deserialize)]
pub struct Author {
    /// Sender of a direct message, scoped to this bot.
    #[serde(default)]
    pub user_openid: Option<String>,
    /// Sender of a group message, scoped to this bot and group.
    #[serde(default)]
    pub member_openid: Option<String>,
    /// Display name, often empty.
    #[serde(default)]
    pub username: Option<String>,
}

/// The kind of conversation an event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// A direct message: `c2c:<user_openid>`.
    Direct,
    /// A group: `group:<group_openid>`.
    Group,
}

/// An inbound text message, ready for the owner check.
#[derive(Debug, PartialEq, Eq)]
pub struct InboundText {
    /// Message id, for dedup and the passive reply.
    pub id: String,
    /// Conversation id the provider's send path accepts.
    pub conversation_id: String,
    /// Sender id for the owner book.
    pub sender: String,
    /// Sender's display name, when QQ sent one.
    pub label: Option<String>,
    /// Trimmed text.
    pub text: String,
    /// Direct or group.
    pub scene: Scene,
}

/// Why a message event is not an inbound text message.
#[derive(Debug, PartialEq, Eq)]
pub enum Skipped {
    /// No text: an attachment, card, quote, or empty message.
    NotText,
    /// The event lacks the sender or group id its scene needs.
    MissingSender,
}

impl MessageEvent {
    /// The conversation, sender, and trimmed text of a text message.
    ///
    /// # Errors
    ///
    /// [`Skipped`] when the event carries no text or no sender.
    pub fn into_text(self, scene: Scene) -> Result<InboundText, Skipped> {
        let text = self.content.trim();
        if text.is_empty() || self.message_type.is_some_and(|kind| kind != 0) {
            return Err(Skipped::NotText);
        }
        let (conversation_id, sender) = match scene {
            Scene::Direct => {
                let sender = self
                    .author
                    .user_openid
                    .filter(|id| !id.is_empty())
                    .ok_or(Skipped::MissingSender)?;
                (alloc::format!("c2c:{sender}"), sender)
            }
            Scene::Group => {
                let group = self
                    .group_openid
                    .filter(|id| !id.is_empty())
                    .ok_or(Skipped::MissingSender)?;
                let sender = self
                    .author
                    .member_openid
                    .filter(|id| !id.is_empty())
                    .ok_or(Skipped::MissingSender)?;
                (alloc::format!("group:{group}"), sender)
            }
        };
        Ok(InboundText {
            text: text.into(),
            id: self.id,
            conversation_id,
            sender,
            label: self.author.username.filter(|name| !name.trim().is_empty()),
            scene,
        })
    }
}

#[derive(Serialize)]
struct Outgoing<T> {
    op: u8,
    d: T,
}

#[derive(Serialize)]
struct Identify<'a> {
    token: &'a str,
    intents: u32,
    shard: [u32; 2],
    properties: Properties,
}

#[derive(Serialize)]
struct Properties {
    #[serde(rename = "$os")]
    os: &'static str,
    #[serde(rename = "$browser")]
    browser: &'static str,
    #[serde(rename = "$device")]
    device: &'static str,
}

#[derive(Serialize)]
struct Resume<'a> {
    token: &'a str,
    session_id: &'a str,
    seq: u64,
}

/// The Identify payload for `access_token`, with [`INTENTS`] and one shard.
///
/// # Errors
///
/// Fails only when JSON encoding fails.
pub fn identify(access_token: &str) -> Result<String, serde_json::Error> {
    let token = alloc::format!("QQBot {access_token}");
    serde_json::to_string(&Outgoing {
        op: op::IDENTIFY,
        d: Identify {
            token: &token,
            intents: INTENTS,
            shard: [0, 1],
            properties: Properties {
                os: "barracuda",
                browser: "barracuda",
                device: "barracuda",
            },
        },
    })
}

/// The Resume payload for a session and the last sequence number seen.
///
/// # Errors
///
/// Fails only when JSON encoding fails.
pub fn resume(access_token: &str, session_id: &str, seq: u64) -> Result<String, serde_json::Error> {
    let token = alloc::format!("QQBot {access_token}");
    serde_json::to_string(&Outgoing {
        op: op::RESUME,
        d: Resume {
            token: &token,
            session_id,
            seq,
        },
    })
}

/// The heartbeat payload with the last sequence number, or `null` before any.
///
/// # Errors
///
/// Fails only when JSON encoding fails.
pub fn heartbeat(seq: Option<u64>) -> Result<String, serde_json::Error> {
    serde_json::to_string(&Outgoing {
        op: op::HEARTBEAT,
        d: seq,
    })
}

/// What to do after the gateway closed the connection with a code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// Reconnect and resume the session (4009, or a drop without a code).
    Resume,
    /// Reconnect with a new Identify; the session is gone.
    Identify,
    /// Stop: the bot may not connect. The message says why.
    Halt(&'static str),
}

/// Message shown when QQ delisted the bot (close code 4914).
pub const DELISTED: &str =
    "QQ 机器人已下架，只能连接沙箱环境 / The QQ bot is delisted and may only use the sandbox";
/// Message shown when QQ banned the bot (close code 4915).
pub const BANNED: &str = "QQ 机器人已被封禁 / The QQ bot is banned";

/// Maps a gateway close code to the next step.
#[must_use]
pub const fn close_action(code: u16) -> CloseAction {
    match code {
        4009 => CloseAction::Resume,
        4914 => CloseAction::Halt(DELISTED),
        4915 => CloseAction::Halt(BANNED),
        _ => CloseAction::Identify,
    }
}

/// `GET /gateway` answer.
#[derive(Deserialize)]
pub(crate) struct GatewayUrl {
    pub(crate) url: String,
}
