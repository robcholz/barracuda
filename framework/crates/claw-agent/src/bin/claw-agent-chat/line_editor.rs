use std::borrow::Cow;
use std::sync::mpsc::{self, Sender};
use std::thread;

use anstyle::{AnsiColor, Style};
use anyhow::{anyhow, Result};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Config, Context, Editor, ExternalPrinter, Helper};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::command::command_hint;

const SAVE_CURSOR: &str = "\u{1b}[s";
const RESTORE_CURSOR: &str = "\u{1b}[u";
const CLEAR_LINE: &str = "\r\u{1b}[2K";

pub(super) enum LineInput {
    Line(String),
    Interrupted,
    Eof,
    Failed(ReadlineError),
}

/// Bridges rustyline's blocking editor into the async session event loop.
pub(super) struct ChatLineEditor {
    prompt: Sender<()>,
    input: UnboundedReceiver<LineInput>,
    printer: Box<dyn ExternalPrinter + Send>,
    stream_cursor_saved: bool,
    waiting_dots: Option<u8>,
}

impl ChatLineEditor {
    pub(super) fn new() -> Result<Self> {
        let config = Config::builder().auto_add_history(true).build();
        let mut editor = Editor::<CommandHelper, DefaultHistory>::with_config(config)?;
        editor.set_helper(Some(CommandHelper));
        let printer = Box::new(editor.create_external_printer()?);
        let (prompt_tx, prompt_rx) = mpsc::channel();
        let (input_tx, input_rx) = unbounded_channel();

        // One thread owns the editor for its entire lifetime. The async loop
        // sends one token whenever the REPL is ready for another input.
        thread::Builder::new()
            .name("claw-agent-chat-input".to_string())
            .spawn(move || {
                while prompt_rx.recv().is_ok() {
                    let (input, terminal) = match editor.readline("> ") {
                        Ok(line) => (LineInput::Line(line), false),
                        Err(ReadlineError::Interrupted) => (LineInput::Interrupted, false),
                        Err(ReadlineError::Eof) => (LineInput::Eof, true),
                        Err(error) => (LineInput::Failed(error), true),
                    };
                    if input_tx.send(input).is_err() || terminal {
                        break;
                    }
                }
            })?;

        Ok(Self {
            prompt: prompt_tx,
            input: input_rx,
            printer,
            stream_cursor_saved: false,
            waiting_dots: None,
        })
    }

    pub(super) fn show_prompt(&self) -> Result<()> {
        self.prompt
            .send(())
            .map_err(|_| anyhow!("line editor stopped"))
    }

    pub(super) async fn next_input(&mut self) -> Option<LineInput> {
        self.input.recv().await
    }

    pub(super) fn print(&mut self, mut message: String) -> Result<()> {
        self.stream_cursor_saved = false;
        if self.waiting_dots.take().is_some() {
            message.insert_str(0, &format!("{RESTORE_CURSOR}{CLEAR_LINE}"));
        }
        if !message.ends_with('\n') {
            message.push('\n');
        }
        self.printer.print(message)?;
        Ok(())
    }

    /// Append one streaming fragment above the active readline prompt.
    ///
    /// `ExternalPrinter` redraws the prompt after every print and forces a
    /// newline. Saving the stream cursor before that newline lets the next
    /// fragment resume at the previous fragment's exact endpoint.
    pub(super) fn print_stream_fragment(&mut self, fragment: &str) -> Result<()> {
        if fragment.is_empty() {
            return Ok(());
        }
        let replace_waiting = self.waiting_dots.take().is_some();
        let message = stream_fragment_message(fragment, self.stream_cursor_saved, replace_waiting);
        self.printer.print(message)?;
        self.stream_cursor_saved = true;
        Ok(())
    }

    pub(super) fn finish_stream(&mut self) {
        self.stream_cursor_saved = false;
    }

    pub(super) fn finish_stream_line(&mut self) -> Result<()> {
        let Some(message) = finish_stream_line_message(self.stream_cursor_saved) else {
            return Ok(());
        };
        self.printer.print(message)?;
        Ok(())
    }

    /// Stop in-place updates after the user commits or interrupts an input
    /// line. Later output starts below that input instead of restoring a cursor
    /// position above it.
    pub(super) fn abandon_live_render(&mut self) {
        self.stream_cursor_saved = false;
        self.waiting_dots = None;
    }

    pub(super) fn start_waiting(&mut self) -> Result<()> {
        if self.waiting_dots.is_some() {
            return Ok(());
        }
        self.stream_cursor_saved = false;
        self.waiting_dots = Some(1);
        self.printer.print(waiting_message(1, false))?;
        Ok(())
    }

    pub(super) fn advance_waiting(&mut self) -> Result<()> {
        let Some(current) = self.waiting_dots else {
            return Ok(());
        };
        let next = current % 3 + 1;
        self.waiting_dots = Some(next);
        self.printer.print(waiting_message(next, true))?;
        Ok(())
    }

    pub(super) fn clear_waiting(&mut self) -> Result<()> {
        if self.waiting_dots.take().is_some() {
            self.printer
                .print(format!("{RESTORE_CURSOR}{CLEAR_LINE}\n"))?;
        }
        Ok(())
    }
}

fn stream_fragment_message(fragment: &str, resume: bool, replace_waiting: bool) -> String {
    let mut message =
        String::with_capacity(fragment.len() + SAVE_CURSOR.len() + RESTORE_CURSOR.len() + 16);
    if resume {
        message.push_str(RESTORE_CURSOR);
    } else if replace_waiting {
        message.push_str(RESTORE_CURSOR);
        message.push_str(CLEAR_LINE);
    }
    message.push_str(fragment);
    message.push_str(SAVE_CURSOR);
    message.push('\n');
    message
}

fn finish_stream_line_message(stream_cursor_saved: bool) -> Option<String> {
    stream_cursor_saved.then(|| stream_fragment_message("\r\n", true, false))
}

fn waiting_message(dots: u8, replace: bool) -> String {
    let style = Style::new()
        .italic()
        .fg_color(Some(AnsiColor::BrightBlack.into()));
    let mut message = String::with_capacity(32);
    if replace {
        message.push_str(RESTORE_CURSOR);
        message.push_str(CLEAR_LINE);
    } else {
        message.push_str(SAVE_CURSOR);
    }
    message.push_str(&format!(
        "{style}waiting{}{style:#}\n",
        ".".repeat(usize::from(dots))
    ));
    message
}

struct CommandHint(String);

impl Hint for CommandHint {
    fn display(&self) -> &str {
        &self.0
    }

    fn completion(&self) -> Option<&str> {
        None
    }
}

struct CommandHelper;

impl Completer for CommandHelper {
    type Candidate = Pair;
}

impl Hinter for CommandHelper {
    type Hint = CommandHint;

    fn hint(&self, line: &str, cursor: usize, _context: &Context<'_>) -> Option<Self::Hint> {
        command_hint(line, cursor).map(CommandHint)
    }
}

impl Highlighter for CommandHelper {
    fn highlight_hint<'hint>(&self, hint: &'hint str) -> Cow<'hint, str> {
        let gray = Style::new().fg_color(Some(AnsiColor::BrightBlack.into()));
        Cow::Owned(format!("{gray}{hint}{gray:#}"))
    }
}

impl Validator for CommandHelper {}

impl Helper for CommandHelper {}

#[cfg(test)]
mod tests {
    use rustyline::highlight::Highlighter;

    use super::*;

    #[test]
    fn command_hint_is_rendered_in_gray() {
        let rendered = CommandHelper.highlight_hint("permissions <deny|ask|allow_all>");

        assert!(rendered.starts_with("\u{1b}[90m"));
        assert!(rendered.ends_with("\u{1b}[0m"));
    }

    #[test]
    fn streaming_fragments_resume_at_the_saved_cursor() {
        assert_eq!(
            stream_fragment_message("Hel", false, false),
            "Hel\u{1b}[s\n"
        );
        assert_eq!(
            stream_fragment_message("lo", true, false),
            "\u{1b}[ulo\u{1b}[s\n"
        );
        assert_eq!(
            stream_fragment_message("Hi", false, true),
            "\u{1b}[u\r\u{1b}[2KHi\u{1b}[s\n"
        );
    }

    #[test]
    fn abandoned_streams_do_not_emit_terminal_blank_lines() {
        assert_eq!(finish_stream_line_message(false), None);
        assert_eq!(
            finish_stream_line_message(true),
            Some("\u{1b}[u\r\n\u{1b}[s\n".to_owned())
        );
    }

    #[test]
    fn waiting_indicator_cycles_one_to_three_dots() {
        let first = waiting_message(1, false);
        let second = waiting_message(2, true);
        let third = waiting_message(3, true);

        assert!(first.contains("\u{1b}[3m"));
        assert!(first.contains("\u{1b}[90m"));
        assert!(first.contains("waiting."));
        assert!(!first.contains("waiting.."));
        assert!(second.contains("waiting.."));
        assert!(third.contains("waiting..."));
        assert!(second.starts_with(&format!("{RESTORE_CURSOR}{CLEAR_LINE}")));
    }
}
