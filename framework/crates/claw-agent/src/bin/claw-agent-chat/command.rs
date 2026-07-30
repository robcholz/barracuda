use claw_agent::{PermissionLevel, ReasoningEffort};
use strum::{EnumString, IntoStaticStr};

const PERMISSIONS_COMMAND: &str = "/permissions";
const PERMISSION_LEVELS: &str = "<deny|ask|allow_all>";
const REASONING_EFFORT_COMMAND: &str = "/reasoning_effort";
const REASONING_EFFORT_LEVELS: &str = "<low|medium|high|ultra>";

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

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CliInput<'a> {
    Message(&'a str),
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
        _ => Err(CommandParseError::UnknownCommand(command.to_string())),
    }
}

pub(super) fn command_hint(line: &str, cursor: usize) -> Option<String> {
    if cursor != line.len() || !line.starts_with('/') {
        return None;
    }
    if line == "/" {
        return Some(format!(
            "permissions {PERMISSION_LEVELS} | reasoning_effort {REASONING_EFFORT_LEVELS}"
        ));
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
    }

    #[test]
    fn slash_input_has_contextual_hints() {
        assert_eq!(command_hint("", 0), None);
        assert_eq!(
            command_hint("/", 1).as_deref(),
            Some("permissions <deny|ask|allow_all> | reasoning_effort <low|medium|high|ultra>")
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
