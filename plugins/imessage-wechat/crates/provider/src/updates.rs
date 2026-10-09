//! iLink `getupdates` long poll: the inbound half of the WeChat channel.
//!
//! The caller owns the connection (a receive-slot lease), the deadline, and
//! the exchange; this module builds the request (path, headers, body) and
//! parses the reply into the few fields the channel uses. Message ids are
//! `u64` on the wire and are kept as decimal strings.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use serde::de::{self, Deserializer, IgnoredAny, Visitor};
use serde::{Deserialize, Serialize};

use crate::WechatConfig;

/// Path of the long poll below the configured API base.
pub const UPDATES_PATH: &str = "/ilink/bot/getupdates";

/// Long-poll duration iLink uses until a reply names its own
/// `longpolling_timeout_ms`.
pub const DEFAULT_LONG_POLL_MS: u64 = 35_000;

/// Largest `getupdates` reply body buffered; a larger batch is skipped
/// ([`parse_oversized_updates`]).
pub const MAX_UPDATES_BYTES: usize = 32 * 1024;

/// Bytes kept from the end of a reply over [`MAX_UPDATES_BYTES`]: room for
/// the `get_updates_buf` and `longpolling_timeout_ms` that follow `msgs`.
pub const MAX_UPDATES_TAIL_BYTES: usize = 4 * 1024;

/// Buffer for the reply's status line and headers.
pub const UPDATES_HEADER_BYTES: usize = 2 * 1024;

/// iLink error code of an expired bot session; only a new QR login helps.
pub const SESSION_EXPIRED_CODE: i64 = -14;

/// `message_type` of a message the user sent; the bot's own are `2`.
const USER_MESSAGE: u32 = 1;

/// `item_list[].type` of a text item.
const TEXT_ITEM: u32 = 1;

/// One text message a user sent to the bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundText {
    /// iLink `message_id` (a `u64`) as a decimal string, or `seq:<n>` when
    /// the message has none.
    pub message_id: String,
    /// Sender (`…@im.wechat`); replies go to this `to_user_id`.
    pub from_user_id: String,
    /// Token iLink expects on replies to this user, when present.
    pub context_token: Option<String>,
    /// The message's text items, joined.
    pub text: String,
}

/// One `getupdates` reply.
#[derive(Debug, Default)]
pub struct Updates {
    /// Text messages from users, in delivery order.
    pub messages: Vec<InboundText>,
    /// Messages skipped: the bot's own, without text, from a group, or
    /// without a sender or id.
    pub skipped: usize,
    /// The cursor to send next, when the reply carried a non-empty one.
    pub get_updates_buf: Option<String>,
    /// The long-poll duration iLink asks for next, when positive.
    pub longpolling_timeout_ms: Option<u64>,
}

/// Why a poll failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesError {
    /// `ret` or `errcode` -14: the bot session expired and needs a new QR
    /// login.
    SessionExpired,
    /// iLink answered with another error code.
    Platform {
        /// `ret` or `errcode`.
        code: i64,
        /// `errmsg`, when present.
        message: String,
    },
    /// A non-2xx HTTP status.
    Status(u16),
    /// The reply body exceeded [`MAX_UPDATES_BYTES`] and its last
    /// [`MAX_UPDATES_TAIL_BYTES`] held no cursor to skip it with.
    TooLarge,
    /// The reply was not the expected JSON.
    Invalid(String),
    /// Writing the request or reading the reply failed. The connection must
    /// be reopened.
    Transport(String),
}

impl fmt::Display for UpdatesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionExpired => formatter.write_str("the WeChat bot session expired"),
            Self::Platform { code, message } => {
                write!(formatter, "iLink error {code}: {message}")
            }
            Self::Status(status) => write!(formatter, "iLink answered HTTP {status}"),
            Self::TooLarge => write!(
                formatter,
                "iLink update batch exceeds {} KiB",
                MAX_UPDATES_BYTES / 1024
            ),
            Self::Invalid(message) => write!(formatter, "invalid iLink update batch: {message}"),
            Self::Transport(message) => write!(formatter, "iLink connection failed: {message}"),
        }
    }
}

/// Encodes the `X-WECHAT-UIN` header for one request: the base64 of the
/// decimal string of a random `u32`.
#[must_use]
pub fn wechat_uin(value: u32) -> String {
    base64(value.to_string().as_bytes())
}

/// The `getupdates` request body for `get_updates_buf` (`""` on first use).
///
/// # Errors
///
/// Returns the encoder's error, which a string body cannot produce.
pub fn updates_body(get_updates_buf: &str) -> Result<Vec<u8>, serde_json::Error> {
    #[derive(Serialize)]
    struct BaseInfo {
        channel_version: &'static str,
    }
    #[derive(Serialize)]
    struct Body<'a> {
        get_updates_buf: &'a str,
        base_info: BaseInfo,
    }
    serde_json::to_vec(&Body {
        get_updates_buf,
        base_info: BaseInfo {
            channel_version: crate::CHANNEL_VERSION,
        },
    })
}

/// The `Authorization` header value of `config`'s bot.
#[must_use]
pub fn authorization(config: &WechatConfig) -> String {
    format!("Bearer {}", config.token)
}

/// Adds the headers of one `getupdates` request: `authorization` from
/// [`authorization`] and `uin` from [`wechat_uin`], a new one per request.
pub fn updates_headers<'a>(
    config: &'a WechatConfig,
    authorization: &'a str,
    uin: &'a str,
    headers: &mut Vec<(&'a str, &'a str)>,
) {
    headers.extend_from_slice(&[
        ("Content-Type", "application/json"),
        ("AuthorizationType", "ilink_bot_token"),
        ("Authorization", authorization),
        ("X-WECHAT-UIN", uin),
        ("iLink-App-Id", config.app_id.as_str()),
        ("iLink-App-ClientVersion", config.client_version.as_str()),
    ]);
    if let Some(route_tag) = &config.route_tag {
        headers.push(("SKRouteTag", route_tag.as_str()));
    }
}

/// Reads a reply larger than [`MAX_UPDATES_BYTES`] whose messages cannot be
/// parsed: from `prefix` (its first bytes) and `tail` (its last bytes), only
/// the cursor and long-poll timeout are kept, so the next poll moves past the
/// batch. The returned [`Updates`] has no messages.
///
/// # Errors
///
/// Returns [`UpdatesError::Status`] for a non-2xx status and
/// [`UpdatesError::TooLarge`] when neither part holds a `get_updates_buf`.
pub fn parse_oversized_updates(
    status: u16,
    prefix: &[u8],
    tail: &[u8],
) -> Result<Updates, UpdatesError> {
    if !(200..300).contains(&status) {
        return Err(UpdatesError::Status(status));
    }
    let field = |name: &str| top_level_value(tail, name).or_else(|| top_level_value(prefix, name));
    let get_updates_buf = field("get_updates_buf")
        .and_then(|value| serde_json::from_slice::<String>(value).ok())
        .filter(|cursor| !cursor.is_empty())
        .ok_or(UpdatesError::TooLarge)?;
    let longpolling_timeout_ms = field("longpolling_timeout_ms")
        .and_then(|value| serde_json::from_slice::<u64>(value).ok())
        .filter(|timeout| *timeout > 0);
    Ok(Updates {
        get_updates_buf: Some(get_updates_buf),
        longpolling_timeout_ms,
        ..Updates::default()
    })
}

/// The JSON value (a string with its quotes, or a run of digits) of the last
/// `"name":` key in `bytes`. A key cannot appear inside a string value, where
/// its quotes would be escaped.
fn top_level_value<'b>(bytes: &'b [u8], name: &str) -> Option<&'b [u8]> {
    let key = format!("\"{name}\"");
    let key = key.as_bytes();
    let mut end = bytes.len();
    while end >= key.len() {
        let start = bytes
            .get(..end)?
            .windows(key.len())
            .rposition(|window| window == key)?;
        end = start.saturating_add(key.len()).saturating_sub(1);
        let rest = bytes.get(start.saturating_add(key.len())..)?;
        let rest = trim_start(rest);
        let Some(rest) = rest.strip_prefix(b":") else {
            continue;
        };
        let rest = trim_start(rest);
        return match rest.first()? {
            b'"' => {
                let mut escaped = false;
                let close = rest.iter().skip(1).position(|byte| {
                    let close = *byte == b'"' && !escaped;
                    escaped = *byte == b'\\' && !escaped;
                    close
                })?;
                rest.get(..close.saturating_add(2))
            }
            _ => {
                let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
                (digits > 0).then(|| rest.get(..digits)).flatten()
            }
        };
    }
    None
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    let blank = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    bytes.get(blank..).unwrap_or_default()
}

/// Parses one `getupdates` reply.
///
/// # Errors
///
/// Returns [`UpdatesError::SessionExpired`] for code -14, another
/// [`UpdatesError`] for a failed reply.
pub fn parse_updates(status: u16, body: &[u8]) -> Result<Updates, UpdatesError> {
    let reply = serde_json::from_slice::<RawUpdates>(body);
    if let Ok(reply) = &reply {
        if let Some(code) = reply.error_code() {
            if code == SESSION_EXPIRED_CODE {
                return Err(UpdatesError::SessionExpired);
            }
            return Err(UpdatesError::Platform {
                code,
                message: reply.errmsg.clone().unwrap_or_default(),
            });
        }
    }
    if !(200..300).contains(&status) {
        return Err(UpdatesError::Status(status));
    }
    let reply = reply.map_err(|error| UpdatesError::Invalid(error.to_string()))?;
    let mut updates = Updates {
        get_updates_buf: reply.get_updates_buf.filter(|cursor| !cursor.is_empty()),
        longpolling_timeout_ms: reply.longpolling_timeout_ms.filter(|timeout| *timeout > 0),
        ..Updates::default()
    };
    for message in reply.msgs {
        match message.into_text() {
            Some(text) => updates.messages.push(text),
            None => updates.skipped = updates.skipped.saturating_add(1),
        }
    }
    Ok(updates)
}

#[derive(Deserialize)]
struct RawUpdates {
    #[serde(default)]
    ret: Option<i64>,
    #[serde(default)]
    errcode: Option<i64>,
    #[serde(default)]
    errmsg: Option<String>,
    #[serde(default)]
    msgs: Vec<RawMessage>,
    #[serde(default)]
    get_updates_buf: Option<String>,
    #[serde(default)]
    longpolling_timeout_ms: Option<u64>,
}

impl RawUpdates {
    /// The first non-zero `ret` or `errcode`, with -14 taking precedence.
    fn error_code(&self) -> Option<i64> {
        let codes = [self.ret, self.errcode];
        if codes.contains(&Some(SESSION_EXPIRED_CODE)) {
            return Some(SESSION_EXPIRED_CODE);
        }
        codes.into_iter().flatten().find(|code| *code != 0)
    }
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default, deserialize_with = "lossless_id")]
    message_id: Option<String>,
    #[serde(default, deserialize_with = "lossless_id")]
    seq: Option<String>,
    #[serde(default)]
    from_user_id: Option<String>,
    #[serde(default)]
    message_type: Option<u32>,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    context_token: Option<String>,
    #[serde(default)]
    item_list: Vec<RawItem>,
}

impl RawMessage {
    fn into_text(self) -> Option<InboundText> {
        if self.message_type != Some(USER_MESSAGE)
            || self
                .group_id
                .as_deref()
                .is_some_and(|group| !group.is_empty())
        {
            return None;
        }
        let from_user_id = self.from_user_id.filter(|sender| !sender.is_empty())?;
        let message_id = self
            .message_id
            .or_else(|| self.seq.map(|seq| format!("seq:{seq}")))?;
        let mut text = String::new();
        for item in self.item_list {
            if item.kind != Some(TEXT_ITEM) {
                continue;
            }
            if let Some(part) = item.text_item.and_then(|text| text.text) {
                text.push_str(&part);
            }
        }
        if text.trim().is_empty() {
            return None;
        }
        Some(InboundText {
            message_id,
            from_user_id,
            context_token: self.context_token.filter(|token| !token.is_empty()),
            text,
        })
    }
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default, rename = "type")]
    kind: Option<u32>,
    #[serde(default)]
    text_item: Option<RawText>,
}

#[derive(Deserialize)]
struct RawText {
    #[serde(default)]
    text: Option<String>,
}

/// Reads an id that iLink sends as a JSON number (`u64`) or a string, as a
/// decimal string, so no precision is lost.
fn lossless_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    struct Id;

    impl<'de> Visitor<'de> for Id {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an integer or string id")
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok((!value.is_empty()).then(|| value.into()))
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(self, inner: D) -> Result<Self::Value, D::Error> {
            inner.deserialize_any(self)
        }

        fn visit_f64<E: de::Error>(self, _value: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_bool<E: de::Error>(self, _value: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(Id)
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |index: u32| {
        let index = usize::try_from(index & 0x3f).unwrap_or_default();
        char::from(ALPHABET.get(index).copied().unwrap_or(b'='))
    };
    let mut encoded = String::new();
    for group in bytes.chunks(3) {
        let mut word = 0_u32;
        for (position, byte) in group.iter().enumerate() {
            let shift =
                16_u32.saturating_sub(u32::try_from(position).unwrap_or(0).saturating_mul(8));
            word |= u32::from(*byte) << shift;
        }
        encoded.push(symbol(word >> 18));
        encoded.push(symbol(word >> 12));
        if group.len() > 1 {
            encoded.push(symbol(word >> 6));
        } else {
            encoded.push('=');
        }
        if group.len() > 2 {
            encoded.push(symbol(word));
        } else {
            encoded.push('=');
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn uin_is_base64_of_the_decimal_string() {
        assert_eq!(wechat_uin(0), "MA==");
        assert_eq!(wechat_uin(12), "MTI=");
        assert_eq!(wechat_uin(123), "MTIz");
        assert_eq!(wechat_uin(u32::MAX), "NDI5NDk2NzI5NQ==");
    }

    #[test]
    fn keeps_u64_ids_exact_and_text_messages_only() {
        let body = br#"{"ret":0,"get_updates_buf":"cursor-2","longpolling_timeout_ms":30000,"msgs":[
            {"seq":1,"message_id":18446744073709551615,"from_user_id":"u1@im.wechat","to_user_id":"b@im.bot",
             "message_type":1,"context_token":"ctx-1","item_list":[{"type":1,"text_item":{"text":"hi"}}]},
            {"message_id":7,"from_user_id":"b@im.bot","message_type":2,"item_list":[{"type":1,"text_item":{"text":"echo"}}]},
            {"message_id":"8","from_user_id":"u1@im.wechat","message_type":1,"item_list":[{"type":2,"image_item":{"url":"x"}}]},
            {"message_id":9,"from_user_id":"u2@im.wechat","message_type":1,"group_id":"g","item_list":[{"type":1,"text_item":{"text":"group"}}]}
        ]}"#;

        let updates = parse_updates(200, body).expect("parses");

        assert_eq!(
            updates.messages,
            [InboundText {
                message_id: "18446744073709551615".into(),
                from_user_id: "u1@im.wechat".into(),
                context_token: Some("ctx-1".into()),
                text: "hi".into(),
            }]
        );
        assert_eq!(updates.skipped, 3);
        assert_eq!(updates.get_updates_buf.as_deref(), Some("cursor-2"));
        assert_eq!(updates.longpolling_timeout_ms, Some(30_000));
    }

    #[test]
    fn empty_cursor_and_missing_fields_are_absent() {
        let updates = parse_updates(200, br#"{"ret":0,"get_updates_buf":""}"#).expect("parses");
        assert!(updates.messages.is_empty());
        assert_eq!(updates.get_updates_buf, None);
        assert_eq!(updates.longpolling_timeout_ms, None);
    }

    #[test]
    fn classifies_errors() {
        assert_eq!(
            parse_updates(200, br#"{"ret":-14,"errmsg":"session timeout"}"#).err(),
            Some(UpdatesError::SessionExpired)
        );
        assert_eq!(
            parse_updates(200, br#"{"ret":0,"errcode":-14}"#).err(),
            Some(UpdatesError::SessionExpired)
        );
        assert_eq!(
            parse_updates(200, br#"{"ret":-1,"errmsg":"busy"}"#).err(),
            Some(UpdatesError::Platform {
                code: -1,
                message: "busy".into()
            })
        );
        assert_eq!(
            parse_updates(502, b"bad gateway").err(),
            Some(UpdatesError::Status(502))
        );
        assert!(matches!(
            parse_updates(200, b"not json"),
            Err(UpdatesError::Invalid(_))
        ));
    }

    #[test]
    fn an_oversized_batch_yields_only_its_cursor() {
        let mut body =
            br#"{"ret":0,"msgs":[{"message_id":1,"item_list":[{"type":1,"text_item":{"text":""#
                .to_vec();
        body.resize(body.len() + 40_000, b'x');
        body.extend_from_slice(
            br#"\"get_updates_buf\": not a key"}}]}], "get_updates_buf" : "next\"cursor","longpolling_timeout_ms":30000}"#,
        );
        let split = MAX_UPDATES_BYTES;
        let prefix = &body[..split];
        let tail = &body[body.len() - MAX_UPDATES_TAIL_BYTES..];

        let updates = parse_oversized_updates(200, prefix, tail).expect("cursor found");

        assert!(updates.messages.is_empty());
        assert_eq!(updates.get_updates_buf.as_deref(), Some("next\"cursor"));
        assert_eq!(updates.longpolling_timeout_ms, Some(30_000));
        assert_eq!(
            parse_oversized_updates(200, prefix, b"xxxx\"}}]}]}").err(),
            Some(UpdatesError::TooLarge)
        );
        assert_eq!(
            parse_oversized_updates(503, prefix, tail).err(),
            Some(UpdatesError::Status(503))
        );
    }

    #[test]
    fn a_cursor_first_reply_is_read_from_its_prefix() {
        let prefix = br#"{"get_updates_buf":"early","ret":0,"msgs":[{"x":"#;
        let updates = parse_oversized_updates(200, prefix, b"yyyy\"}]}").expect("cursor found");
        assert_eq!(updates.get_updates_buf.as_deref(), Some("early"));
        assert_eq!(updates.longpolling_timeout_ms, None);
    }

    #[test]
    fn request_headers_carry_the_bot_token_and_a_fresh_uin() {
        let mut config = WechatConfig::new("tok");
        config.route_tag = Some("rt".into());
        let authorization = authorization(&config);
        let mut headers = Vec::new();
        updates_headers(&config, &authorization, "MTI=", &mut headers);
        assert!(headers.contains(&("Authorization", "Bearer tok")));
        assert!(headers.contains(&("X-WECHAT-UIN", "MTI=")));
        assert!(headers.contains(&("AuthorizationType", "ilink_bot_token")));
        assert!(headers.contains(&("SKRouteTag", "rt")));
    }

    #[test]
    fn request_body_carries_the_cursor_and_base_info() {
        let body = updates_body("abc").expect("encodes");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON");
        assert_eq!(json["get_updates_buf"], "abc");
        assert_eq!(json["base_info"]["channel_version"], crate::CHANNEL_VERSION);
    }
}
