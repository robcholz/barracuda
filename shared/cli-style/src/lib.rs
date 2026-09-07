//! Shared terminal styling for Barracuda command-line tools.

use anstyle::{AnsiColor, Color, Style};
use clap::builder::styling::Styles;

/// Consistent help styling for all `cargo` workspace commands.
pub const CLI_STYLES: Styles = Styles::styled()
    .header(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
            .bold(),
    )
    .usage(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Green)))
            .bold(),
    )
    .literal(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
            .bold(),
    )
    .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlack))))
    .error(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Red)))
            .bold(),
    )
    .valid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
    .invalid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))));

/// Style for successful command output.
pub const SUCCESS: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .bold();

/// Style for warnings and disabled items.
pub const WARNING: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));

/// Style for error labels.
pub const ERROR: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();

/// Style for command and domain object names.
pub const EMPHASIS: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
    .bold();

/// Style for secondary information such as paths and counts.
pub const DIM: Style = Style::new().dimmed();

#[cfg(test)]
mod tests {
    use super::{CLI_STYLES, DIM, EMPHASIS, ERROR, SUCCESS, WARNING};

    #[test]
    fn shared_styles_are_color_capable() {
        assert!(!CLI_STYLES.get_header().is_plain());
        assert!(!SUCCESS.is_plain());
        assert!(!WARNING.is_plain());
        assert!(!ERROR.is_plain());
        assert!(!EMPHASIS.is_plain());
        assert!(!DIM.is_plain());
    }
}
