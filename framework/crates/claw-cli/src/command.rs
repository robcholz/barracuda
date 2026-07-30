use claw_agent::{PermissionLevel, ReasoningEffort, SessionId, SessionPersistence};
use strum::{EnumString, IntoStaticStr};

const PERMISSIONS_COMMAND: &str = "/permissions";
const PERMISSION_LEVELS: &str = "<deny|ask|allow_all>";
const REASONING_EFFORT_COMMAND: &str = "/reasoning_effort";
const REASONING_EFFORT_LEVELS: &str = "<low|medium|high|ultra>";
const APPEND_COMMAND: &str = "/append";
const APPEND_MESSAGE: &str = "<message>";
const INTERRUPT_COMMAND: &str = "/interrupt";
const CANCEL_COMMAND: &str = "/cancel";
const SESSION_COMMAND: &str = "/session";
const SESSION_SUBCOMMANDS: &str = "<new|resume|delete>";
const SESSION_ID: &str = "<session_id>";
const SESSION_PERSISTENCE: &str = "[persistent|ephemeral]";

#[derive(Clone, Copy, Debug, EnumString, IntoStaticStr, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
pub(super) enum PermissionLevelArg {
    Deny,
    Ask,
    AllowAll,
}

impl From<PermissionLevelArg> for PermissionLevel {
    fn from(level: PermissionLevelArg) -> Self {
        match level {
            PermissionLevelArg::Deny => Self::Deny,
            PermissionLevelArg::Ask => Self::Ask,
            PermissionLevelArg::AllowAll => Self::AllowAll,
        }
    }
}

#[derive(Clone, Copy, Debug, EnumString, IntoStaticStr, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
pub(super) enum ReasoningEffortArg {
    Low,
    Medium,
    High,
    Ultra,
}

impl From<ReasoningEffortArg> for ReasoningEffort {
    fn from(effort: ReasoningEffortArg) -> Self {
        match effort {
            ReasoningEffortArg::Low => Self::Low,
            ReasoningEffortArg::Medium => Self::Medium,
            ReasoningEffortArg::High => Self::High,
            ReasoningEffortArg::Ultra => Self::Ultra,
        }
    }
}

#[derive(Clone, Copy, Debug, EnumString, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
enum SessionSubcommandArg {
    New,
    Resume,
    Delete,
}

#[derive(Clone, Copy, Debug, Default, EnumString, IntoStaticStr, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
pub(super) enum SessionPersistenceArg {
    #[default]
    Persistent,
    Ephemeral,
}

impl From<SessionPersistenceArg> for SessionPersistence {
    fn from(persistence: SessionPersistenceArg) -> Self {
        match persistence {
            SessionPersistenceArg::Persistent => Self::Persistent,
            SessionPersistenceArg::Ephemeral => Self::Ephemeral,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CliInput<'a> {
    Message(&'a str),
    Append(&'a str),
    Interrupt,
    Cancel,
    SessionNew(SessionPersistenceArg),
    SessionResume(SessionId),
    SessionDelete(Option<SessionId>),
    SetPermission(PermissionLevelArg),
    SetReasoningEffort(ReasoningEffortArg),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum CommandParseError {
    #[error("unknown command '/{0}'")]
    UnknownCommand(String),
    #[error("usage: /permissions {PERMISSION_LEVELS}")]
    MissingPermissionLevel,
    #[error("unknown permission level '{0}'; expected deny, ask, or allow_all")]
    UnknownPermissionLevel(String),
    #[error("unexpected argument '{0}'; usage: /permissions {PERMISSION_LEVELS}")]
    UnexpectedArgument(String),
    #[error("usage: /reasoning_effort {REASONING_EFFORT_LEVELS}")]
    MissingReasoningEffort,
    #[error("unknown reasoning effort '{0}'; expected low, medium, high, or ultra")]
    UnknownReasoningEffort(String),
    #[error("unexpected argument '{0}'; usage: /reasoning_effort {REASONING_EFFORT_LEVELS}")]
    UnexpectedReasoningEffortArgument(String),
    #[error("usage: /append {APPEND_MESSAGE}")]
    MissingAppendMessage,
    #[error("unexpected argument '{argument}'; usage: /{command}")]
    UnexpectedControlArgument {
        command: &'static str,
        argument: String,
    },
    #[error("usage: /session {SESSION_SUBCOMMANDS}")]
    MissingSessionSubcommand,
    #[error("unknown session command '{0}'; expected new, resume, or delete")]
    UnknownSessionSubcommand(String),
    #[error("usage: /session resume {SESSION_ID}")]
    MissingResumeSessionId,
    #[error("invalid session id '{value}' for /session {command}; expected session-N")]
    InvalidSessionId {
        command: &'static str,
        value: String,
    },
    #[error("unexpected argument '{argument}'; usage: /session {usage}")]
    UnexpectedSessionArgument {
        usage: &'static str,
        argument: String,
    },
    #[error("unknown session persistence '{0}'; expected persistent or ephemeral")]
    UnknownSessionPersistence(String),
}

impl CommandParseError {
    pub(super) fn should_show_session_list(&self) -> bool {
        matches!(
            self,
            Self::MissingResumeSessionId
                | Self::InvalidSessionId {
                    command: "resume",
                    ..
                }
        )
    }
}

pub(super) fn parse_input(input: &str) -> Result<CliInput<'_>, CommandParseError> {
    let input = input.trim();
    let Some(command_line) = input.strip_prefix('/') else {
        return Ok(CliInput::Message(input));
    };
    let mut parts = command_line.split_whitespace();
    let command = parts.next().unwrap_or_default();
    match command {
        "permissions" => {
            let level = parts
                .next()
                .ok_or(CommandParseError::MissingPermissionLevel)?;
            if let Some(argument) = parts.next() {
                return Err(CommandParseError::UnexpectedArgument(argument.to_string()));
            }
            let level = level
                .parse::<PermissionLevelArg>()
                .map_err(|_| CommandParseError::UnknownPermissionLevel(level.to_string()))?;
            Ok(CliInput::SetPermission(level))
        }
        "reasoning_effort" => {
            let effort = parts
                .next()
                .ok_or(CommandParseError::MissingReasoningEffort)?;
            if let Some(argument) = parts.next() {
                return Err(CommandParseError::UnexpectedReasoningEffortArgument(
                    argument.to_string(),
                ));
            }
            let effort = effort
                .parse::<ReasoningEffortArg>()
                .map_err(|_| CommandParseError::UnknownReasoningEffort(effort.to_string()))?;
            Ok(CliInput::SetReasoningEffort(effort))
        }
        "append" => {
            let message = command_line
                .strip_prefix(command)
                .unwrap_or_default()
                .trim();
            if message.is_empty() {
                Err(CommandParseError::MissingAppendMessage)
            } else {
                Ok(CliInput::Append(message))
            }
        }
        "interrupt" => {
            if let Some(argument) = parts.next() {
                return Err(CommandParseError::UnexpectedControlArgument {
                    command: "interrupt",
                    argument: argument.to_string(),
                });
            }
            Ok(CliInput::Interrupt)
        }
        "cancel" => {
            if let Some(argument) = parts.next() {
                return Err(CommandParseError::UnexpectedControlArgument {
                    command: "cancel",
                    argument: argument.to_string(),
                });
            }
            Ok(CliInput::Cancel)
        }
        "session" => {
            let subcommand = parts
                .next()
                .ok_or(CommandParseError::MissingSessionSubcommand)?;
            let subcommand = subcommand
                .parse::<SessionSubcommandArg>()
                .map_err(|_| CommandParseError::UnknownSessionSubcommand(subcommand.to_string()))?;
            match subcommand {
                SessionSubcommandArg::New => {
                    let persistence = parts
                        .next()
                        .map(|persistence| {
                            persistence.parse::<SessionPersistenceArg>().map_err(|_| {
                                CommandParseError::UnknownSessionPersistence(
                                    persistence.to_string(),
                                )
                            })
                        })
                        .transpose()?
                        .unwrap_or_default();
                    if let Some(argument) = parts.next() {
                        return Err(CommandParseError::UnexpectedSessionArgument {
                            usage: "new [persistent|ephemeral]",
                            argument: argument.to_string(),
                        });
                    }
                    Ok(CliInput::SessionNew(persistence))
                }
                SessionSubcommandArg::Resume => {
                    let session = parts
                        .next()
                        .ok_or(CommandParseError::MissingResumeSessionId)?;
                    let parsed = session.parse::<SessionId>().map_err(|_| {
                        CommandParseError::InvalidSessionId {
                            command: "resume",
                            value: session.to_string(),
                        }
                    })?;
                    if let Some(argument) = parts.next() {
                        return Err(CommandParseError::UnexpectedSessionArgument {
                            usage: "resume <session_id>",
                            argument: argument.to_string(),
                        });
                    }
                    Ok(CliInput::SessionResume(parsed))
                }
                SessionSubcommandArg::Delete => {
                    let session = parts
                        .next()
                        .map(|session| {
                            session.parse::<SessionId>().map_err(|_| {
                                CommandParseError::InvalidSessionId {
                                    command: "delete",
                                    value: session.to_string(),
                                }
                            })
                        })
                        .transpose()?;
                    if let Some(argument) = parts.next() {
                        return Err(CommandParseError::UnexpectedSessionArgument {
                            usage: "delete [session_id]",
                            argument: argument.to_string(),
                        });
                    }
                    Ok(CliInput::SessionDelete(session))
                }
            }
        }
        _ => Err(CommandParseError::UnknownCommand(command.to_string())),
    }
}

pub(super) fn command_hint(line: &str, cursor: usize) -> Option<String> {
    if cursor != line.len() || !line.starts_with('/') {
        return None;
    }
    if line == "/" {
        return Some(format!(
            "append {APPEND_MESSAGE} | interrupt | cancel | session {SESSION_SUBCOMMANDS} | permissions {PERMISSION_LEVELS} | reasoning_effort {REASONING_EFFORT_LEVELS}"
        ));
    }
    if let Some(suffix) = APPEND_COMMAND.strip_prefix(line) {
        return Some(format!("{suffix} {APPEND_MESSAGE}"));
    }
    if line.strip_prefix(APPEND_COMMAND) == Some(" ") {
        return Some(APPEND_MESSAGE.to_string());
    }
    if let Some(suffix) = INTERRUPT_COMMAND.strip_prefix(line) {
        return Some(suffix.to_string());
    }
    if let Some(suffix) = CANCEL_COMMAND.strip_prefix(line) {
        return Some(suffix.to_string());
    }
    if let Some(suffix) = SESSION_COMMAND.strip_prefix(line) {
        return Some(format!("{suffix} {SESSION_SUBCOMMANDS}"));
    }
    if let Some(subcommand) = line.strip_prefix("/session ") {
        if subcommand.is_empty() {
            return Some("new | resume <session_id> | delete [session_id]".to_string());
        }
        if let Some(suffix) = "new".strip_prefix(subcommand) {
            return Some(format!("{suffix} {SESSION_PERSISTENCE}"));
        }
        if let Some(persistence) = subcommand.strip_prefix("new ") {
            if persistence.is_empty() {
                return Some(SESSION_PERSISTENCE.to_string());
            }
            if let Some(suffix) = "persistent".strip_prefix(persistence) {
                return Some(suffix.to_string());
            }
            if let Some(suffix) = "ephemeral".strip_prefix(persistence) {
                return Some(suffix.to_string());
            }
        }
        if let Some(suffix) = "resume".strip_prefix(subcommand) {
            return Some(format!("{suffix} {SESSION_ID}"));
        }
        if subcommand == "resume " {
            return Some(SESSION_ID.to_string());
        }
        if let Some(suffix) = "delete".strip_prefix(subcommand) {
            return Some(format!("{suffix} [session_id]"));
        }
        if subcommand == "delete " {
            return Some("[session_id]".to_string());
        }
    }
    if let Some(suffix) = PERMISSIONS_COMMAND.strip_prefix(line) {
        return Some(format!("{suffix} {PERMISSION_LEVELS}"));
    }
    if line.strip_prefix(PERMISSIONS_COMMAND) == Some(" ") {
        return Some("deny | ask | allow_all".to_string());
    }
    if let Some(suffix) = REASONING_EFFORT_COMMAND.strip_prefix(line) {
        return Some(format!("{suffix} {REASONING_EFFORT_LEVELS}"));
    }
    if line.strip_prefix(REASONING_EFFORT_COMMAND) == Some(" ") {
        return Some("low | medium | high | ultra".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_a_message() {
        assert_eq!(
            parse_input("  explain this code  "),
            Ok(CliInput::Message("explain this code"))
        );
    }

    #[test]
    fn permissions_command_parses_every_level() {
        assert_eq!(
            parse_input("/permissions deny"),
            Ok(CliInput::SetPermission(PermissionLevelArg::Deny))
        );
        assert_eq!(
            parse_input("/permissions ask"),
            Ok(CliInput::SetPermission(PermissionLevelArg::Ask))
        );
        assert_eq!(
            parse_input("/permissions allow_all"),
            Ok(CliInput::SetPermission(PermissionLevelArg::AllowAll))
        );
    }

    #[test]
    fn reasoning_effort_command_parses_every_level() {
        assert_eq!(
            parse_input("/reasoning_effort low"),
            Ok(CliInput::SetReasoningEffort(ReasoningEffortArg::Low))
        );
        assert_eq!(
            parse_input("/reasoning_effort medium"),
            Ok(CliInput::SetReasoningEffort(ReasoningEffortArg::Medium))
        );
        assert_eq!(
            parse_input("/reasoning_effort high"),
            Ok(CliInput::SetReasoningEffort(ReasoningEffortArg::High))
        );
        assert_eq!(
            parse_input("/reasoning_effort ultra"),
            Ok(CliInput::SetReasoningEffort(ReasoningEffortArg::Ultra))
        );
    }

    #[test]
    fn session_control_commands_parse() {
        assert_eq!(
            parse_input("/append  keep the queued work  "),
            Ok(CliInput::Append("keep the queued work"))
        );
        assert_eq!(parse_input("/interrupt"), Ok(CliInput::Interrupt));
        assert_eq!(parse_input("/cancel"), Ok(CliInput::Cancel));
    }

    #[test]
    fn session_lifecycle_commands_parse_typed_ids() {
        assert_eq!(
            parse_input("/session new"),
            Ok(CliInput::SessionNew(SessionPersistenceArg::Persistent))
        );
        assert_eq!(
            parse_input("/session new persistent"),
            Ok(CliInput::SessionNew(SessionPersistenceArg::Persistent))
        );
        assert_eq!(
            parse_input("/session new ephemeral"),
            Ok(CliInput::SessionNew(SessionPersistenceArg::Ephemeral))
        );
        assert_eq!(
            parse_input("/session resume session-12"),
            Ok(CliInput::SessionResume(SessionId(12)))
        );
        assert_eq!(
            parse_input("/session delete"),
            Ok(CliInput::SessionDelete(None))
        );
        assert_eq!(
            parse_input("/session delete session-9"),
            Ok(CliInput::SessionDelete(Some(SessionId(9))))
        );
    }

    #[test]
    fn invalid_commands_and_arguments_are_rejected() {
        assert_eq!(
            parse_input("/permissions"),
            Err(CommandParseError::MissingPermissionLevel)
        );
        assert_eq!(
            parse_input("/unknown"),
            Err(CommandParseError::UnknownCommand("unknown".to_string()))
        );
        assert_eq!(
            parse_input("/permissions maybe"),
            Err(CommandParseError::UnknownPermissionLevel(
                "maybe".to_string()
            ))
        );
        assert_eq!(
            parse_input("/permissions allow-all"),
            Err(CommandParseError::UnknownPermissionLevel(
                "allow-all".to_string()
            ))
        );
        assert_eq!(
            parse_input("/permissions ask now"),
            Err(CommandParseError::UnexpectedArgument("now".to_string()))
        );
        assert_eq!(
            parse_input("/reasoning_effort"),
            Err(CommandParseError::MissingReasoningEffort)
        );
        assert_eq!(
            parse_input("/reasoning_effort extreme"),
            Err(CommandParseError::UnknownReasoningEffort(
                "extreme".to_string()
            ))
        );
        assert_eq!(
            parse_input("/reasoning_effort high now"),
            Err(CommandParseError::UnexpectedReasoningEffortArgument(
                "now".to_string()
            ))
        );
        assert_eq!(
            parse_input("/reasoning-effort high"),
            Err(CommandParseError::UnknownCommand(
                "reasoning-effort".to_string()
            ))
        );
        assert_eq!(
            parse_input("/append"),
            Err(CommandParseError::MissingAppendMessage)
        );
        assert_eq!(
            parse_input("/interrupt now"),
            Err(CommandParseError::UnexpectedControlArgument {
                command: "interrupt",
                argument: "now".to_string(),
            })
        );
        assert_eq!(
            parse_input("/cancel now"),
            Err(CommandParseError::UnexpectedControlArgument {
                command: "cancel",
                argument: "now".to_string(),
            })
        );
        assert_eq!(
            parse_input("/session"),
            Err(CommandParseError::MissingSessionSubcommand)
        );
        assert_eq!(
            parse_input("/session open session-1"),
            Err(CommandParseError::UnknownSessionSubcommand(
                "open".to_string()
            ))
        );
        assert_eq!(
            parse_input("/session list"),
            Err(CommandParseError::UnknownSessionSubcommand(
                "list".to_string()
            ))
        );
        assert_eq!(
            parse_input("/session resume"),
            Err(CommandParseError::MissingResumeSessionId)
        );
        assert_eq!(
            parse_input("/session resume 7"),
            Err(CommandParseError::InvalidSessionId {
                command: "resume",
                value: "7".to_string(),
            })
        );
        assert_eq!(
            parse_input("/session delete nope"),
            Err(CommandParseError::InvalidSessionId {
                command: "delete",
                value: "nope".to_string(),
            })
        );
        assert_eq!(
            parse_input("/session new now"),
            Err(CommandParseError::UnknownSessionPersistence(
                "now".to_string()
            ))
        );
        assert_eq!(
            parse_input("/session new ephemeral now"),
            Err(CommandParseError::UnexpectedSessionArgument {
                usage: "new [persistent|ephemeral]",
                argument: "now".to_string(),
            })
        );
        assert_eq!(
            parse_input("/session resume session-1 now"),
            Err(CommandParseError::UnexpectedSessionArgument {
                usage: "resume <session_id>",
                argument: "now".to_string(),
            })
        );
    }

    #[test]
    fn resume_id_errors_request_the_session_list() {
        assert!(CommandParseError::MissingResumeSessionId.should_show_session_list());
        assert!(CommandParseError::InvalidSessionId {
            command: "resume",
            value: "invalid".to_string(),
        }
        .should_show_session_list());
        assert!(!CommandParseError::InvalidSessionId {
            command: "delete",
            value: "invalid".to_string(),
        }
        .should_show_session_list());
    }

    #[test]
    fn slash_input_has_contextual_hints() {
        assert_eq!(command_hint("", 0), None);
        assert_eq!(
            command_hint("/", 1).as_deref(),
            Some(
                "append <message> | interrupt | cancel | session <new|resume|delete> | permissions <deny|ask|allow_all> | reasoning_effort <low|medium|high|ultra>"
            )
        );
        assert_eq!(command_hint("/app", 4).as_deref(), Some("end <message>"));
        assert_eq!(command_hint("/append ", 8).as_deref(), Some("<message>"));
        assert_eq!(command_hint("/inter", 6).as_deref(), Some("rupt"));
        assert_eq!(command_hint("/can", 4).as_deref(), Some("cel"));
        assert_eq!(
            command_hint("/sess", 5).as_deref(),
            Some("ion <new|resume|delete>")
        );
        assert_eq!(
            command_hint("/session ", 9).as_deref(),
            Some("new | resume <session_id> | delete [session_id]")
        );
        assert_eq!(
            command_hint("/session n", 10).as_deref(),
            Some("ew [persistent|ephemeral]")
        );
        assert_eq!(
            command_hint("/session new ", 13).as_deref(),
            Some("[persistent|ephemeral]")
        );
        assert_eq!(
            command_hint("/session new p", 14).as_deref(),
            Some("ersistent")
        );
        assert_eq!(
            command_hint("/session new e", 14).as_deref(),
            Some("phemeral")
        );
        assert_eq!(
            command_hint("/session r", 10).as_deref(),
            Some("esume <session_id>")
        );
        assert_eq!(
            command_hint("/session resume ", 16).as_deref(),
            Some("<session_id>")
        );
        assert_eq!(
            command_hint("/session d", 10).as_deref(),
            Some("elete [session_id]")
        );
        assert_eq!(
            command_hint("/perm", 5).as_deref(),
            Some("issions <deny|ask|allow_all>")
        );
        assert_eq!(
            command_hint("/permissions ", 13).as_deref(),
            Some("deny | ask | allow_all")
        );
        assert_eq!(
            command_hint("/reason", 7).as_deref(),
            Some("ing_effort <low|medium|high|ultra>")
        );
        assert_eq!(
            command_hint("/reasoning_effort ", 18).as_deref(),
            Some("low | medium | high | ultra")
        );
        assert_eq!(command_hint("hello", 5), None);
        assert_eq!(command_hint("/permissions ", 4), None);
    }
}
