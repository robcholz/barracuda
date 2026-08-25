//! Terminal command hints.
//!
//! The terminal is an external IM client: user input is ordinary message text.
//! The only local commands are `/quit` and `/exit`, so hinting is minimal.

/// Returns an inline completion hint for a partial local command, if any.
pub(super) fn command_hint(line: &str, cursor: usize) -> Option<String> {
    if cursor != line.len() || !line.starts_with('/') {
        return None;
    }
    if line == "/" {
        return Some("quit | exit".to_string());
    }
    if let Some(suffix) = "/quit".strip_prefix(line) {
        return Some(suffix.to_string());
    }
    if let Some(suffix) = "/exit".strip_prefix(line) {
        return Some(suffix.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::command_hint;

    #[test]
    fn plain_text_is_not_hinted() {
        assert_eq!(command_hint("hello", 5), None);
        assert_eq!(command_hint("", 0), None);
    }

    #[test]
    fn slash_prefix_lists_local_commands() {
        assert_eq!(command_hint("/", 1).as_deref(), Some("quit | exit"));
    }

    #[test]
    fn partial_commands_complete() {
        assert_eq!(command_hint("/qu", 3).as_deref(), Some("it"));
        assert_eq!(command_hint("/ex", 3).as_deref(), Some("it"));
    }

    #[test]
    fn hint_only_at_end_of_line() {
        assert_eq!(command_hint("/quit", 2), None);
    }
}
