use alloc::string::String;

use barracuda_workflow_plugin::Event;
use serde::{Deserialize, Serialize};

use crate::json::valid_required;
use crate::route::GatewayRoute;

/// What a user asked of their conversation's sessions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayControlKind {
    /// Stop the running turn at its next iteration boundary, keeping what it produced.
    Interrupt,
    /// Abort the running turn at once.
    Cancel,
    /// List the conversation's sessions.
    Sessions,
    /// Start a new session with the next message (`temporary` for one that is
    /// never saved and is deleted when the conversation leaves it).
    New,
    /// Continue another of the conversation's sessions.
    Switch,
    /// Rename one of the conversation's sessions to `title`.
    Rename,
    /// Delete one of the conversation's sessions; without `confirm` it only asks.
    Delete,
    /// A malformed session command: answer with the command list.
    Help,
}

/// Gateway-owned control request published by channel providers, or parsed
/// from a text command by [`parse_command`].
///
/// A command names its session either by `session` id (rich channels) or by
/// `index`, its 1-based place in the conversation's newest-first list (text
/// commands).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayInboundControl {
    /// Origin route and conversation identity.
    pub route: GatewayRoute,
    /// The requested control.
    pub control: GatewayControlKind,
    /// `new`: start a temporary session.
    #[serde(default, skip_serializing_if = "is_false")]
    pub temporary: bool,
    /// `switch`, `rename`, `delete`: the session, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `switch`, `rename`, `delete`: the session, by its 1-based list number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// `rename`: the new title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `delete`: the user confirmed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
const fn is_false(value: &bool) -> bool {
    !*value
}

impl GatewayInboundControl {
    /// A control with no arguments.
    #[must_use]
    pub const fn new(route: GatewayRoute, control: GatewayControlKind) -> Self {
        Self {
            route,
            control,
            temporary: false,
            session: None,
            index: None,
            title: None,
            confirm: false,
        }
    }
}

/// Workflow Event emitted for a normalized inbound control request.
pub struct GatewayControlReceived;

impl Event for GatewayControlReceived {
    const ID: &'static str = "gateway.control.received";
}

pub(crate) fn valid_control(control: &GatewayInboundControl) -> bool {
    valid_required(&control.route.channel) && valid_required(&control.route.conversation_id)
}

/// Parses a session text command: `/sessions`, `/new`, `/new temp`,
/// `/switch N`, `/rename N title`, `/delete N`, `/delete N confirm`.
///
/// Any other text, including other slash commands, is an ordinary message
/// (`None`). A known command with bad arguments asks for help.
#[must_use]
pub fn parse_command(route: &GatewayRoute, text: &str) -> Option<GatewayInboundControl> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let (word, arguments) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(word, arguments)| (word, arguments.trim()));
    let control = |kind| GatewayInboundControl::new(route.clone(), kind);
    let help = || Some(control(GatewayControlKind::Help));
    let number = |value: &str| value.parse::<u32>().ok().filter(|index| *index > 0);
    match word.to_ascii_lowercase().as_str() {
        "sessions" if arguments.is_empty() => Some(control(GatewayControlKind::Sessions)),
        "new" => match arguments.to_ascii_lowercase().as_str() {
            "" => Some(control(GatewayControlKind::New)),
            "temp" | "temporary" => Some(GatewayInboundControl {
                temporary: true,
                ..control(GatewayControlKind::New)
            }),
            _ => help(),
        },
        "switch" => match number(arguments) {
            Some(index) => Some(GatewayInboundControl {
                index: Some(index),
                ..control(GatewayControlKind::Switch)
            }),
            None => help(),
        },
        "rename" => {
            let (index, title) = arguments
                .split_once(char::is_whitespace)
                .map_or((arguments, ""), |(index, title)| (index, title.trim()));
            match number(index) {
                Some(index) if !title.is_empty() => Some(GatewayInboundControl {
                    index: Some(index),
                    title: Some(String::from(title)),
                    ..control(GatewayControlKind::Rename)
                }),
                _ => help(),
            }
        }
        "delete" => {
            let (index, confirm) = arguments
                .split_once(char::is_whitespace)
                .map_or((arguments, ""), |(index, confirm)| (index, confirm.trim()));
            match (number(index), confirm.to_ascii_lowercase().as_str()) {
                (Some(index), "") => Some(GatewayInboundControl {
                    index: Some(index),
                    ..control(GatewayControlKind::Delete)
                }),
                (Some(index), "confirm") => Some(GatewayInboundControl {
                    index: Some(index),
                    confirm: true,
                    ..control(GatewayControlKind::Delete)
                }),
                _ => help(),
            }
        }
        "sessions" => help(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::string::String;

    use super::{parse_command, GatewayControlKind, GatewayInboundControl};
    use crate::route::GatewayRoute;

    fn route() -> GatewayRoute {
        GatewayRoute::new("telegram", "chat-1")
    }

    fn kind(text: &str) -> Option<GatewayControlKind> {
        parse_command(&route(), text).map(|control| control.control)
    }

    #[test]
    fn session_commands_become_controls() {
        assert_eq!(kind("/sessions"), Some(GatewayControlKind::Sessions));
        assert_eq!(kind("  /SESSIONS "), Some(GatewayControlKind::Sessions));
        assert_eq!(
            parse_command(&route(), "/new temp"),
            Some(GatewayInboundControl {
                temporary: true,
                ..GatewayInboundControl::new(route(), GatewayControlKind::New)
            })
        );
        assert_eq!(
            parse_command(&route(), "/switch 2").and_then(|control| control.index),
            Some(2)
        );
        let rename = parse_command(&route(), "/rename 3  Weekend ride ").expect("rename");
        assert_eq!(rename.index, Some(3));
        assert_eq!(rename.title, Some(String::from("Weekend ride")));
        let delete = parse_command(&route(), "/delete 1 confirm").expect("delete");
        assert_eq!((delete.index, delete.confirm), (Some(1), true));
        assert!(
            !parse_command(&route(), "/delete 1")
                .expect("delete")
                .confirm
        );
    }

    #[test]
    fn bad_arguments_ask_for_help_and_other_text_is_a_message() {
        for text in [
            "/switch",
            "/switch two",
            "/switch 0",
            "/rename 2",
            "/delete 1 now",
            "/new x",
            "/sessions 2",
        ] {
            assert_eq!(kind(text), Some(GatewayControlKind::Help), "{text}");
        }
        for text in ["hello", "/start", "/newsletter", "a /sessions b", ""] {
            assert_eq!(kind(text), None, "{text}");
        }
    }
}
