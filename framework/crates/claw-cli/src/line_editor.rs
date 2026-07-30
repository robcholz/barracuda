use std::borrow::Cow;
use std::cell::Cell;
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
use rustyline::{Config, Context, Editor, ExternalPrinter, Helper, Prompt};
use terminal_size::{terminal_size, Width};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use unicode_width::UnicodeWidthChar;

use super::command::command_hint;

const MOVE_UP_ONE: &str = "\u{1b}[A";
const CLEAR_LINE: &str = "\r\u{1b}[2K";
const FALLBACK_COLUMNS: usize = 80;
const TAB_STOP: usize = 8;
const PROMPT_COLUMNS: usize = 2;

pub(super) enum LineInput {
    PromptReady,
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
    stream: LiveStream,
    live_row_displayed: bool,
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
                    let prompt = ChatPrompt::new(input_tx.clone());
                    let (input, terminal) = match editor.readline(&prompt) {
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
            stream: LiveStream::default(),
            live_row_displayed: false,
            waiting_dots: None,
        })
    }

    pub(super) async fn show_prompt(&mut self) -> Result<()> {
        self.prompt
            .send(())
            .map_err(|_| anyhow!("line editor stopped"))?;
        match self.input.recv().await {
            Some(LineInput::PromptReady) => Ok(()),
            Some(_) => Err(anyhow!(
                "line editor produced input before prompt was ready"
            )),
            None => Err(anyhow!("line editor stopped before prompt was ready")),
        }
    }

    pub(super) async fn next_input(&mut self) -> Option<LineInput> {
        self.input.recv().await
    }

    pub(super) fn print(&mut self, mut message: String) -> Result<()> {
        self.finish_stream();
        if self.waiting_dots.take().is_some() && self.live_row_displayed {
            message.insert_str(0, &format!("{MOVE_UP_ONE}{CLEAR_LINE}"));
        }
        if !message.ends_with('\n') {
            message.push('\n');
        }
        self.printer.print(normalize_newlines(&message))?;
        self.live_row_displayed = false;
        Ok(())
    }

    /// Append one streaming fragment above the active readline prompt.
    ///
    /// Completed physical rows are committed to terminal history immediately.
    /// Only the unfinished final row is redrawn, so the live render area is
    /// always exactly one row above the readline input area.
    pub(super) fn print_stream_fragment(&mut self, fragment: &str) -> Result<()> {
        if fragment.is_empty() {
            return Ok(());
        }
        self.waiting_dots = None;
        let completed = self.stream.push(fragment, render_columns());
        let message =
            live_update_message(&completed, self.stream.current(), self.live_row_displayed);
        if !message.is_empty() {
            self.printer.print(message)?;
        }
        self.live_row_displayed = !self.stream.current().is_empty();
        Ok(())
    }

    pub(super) fn finish_stream(&mut self) {
        self.stream.clear();
        if self.waiting_dots.is_none() {
            self.live_row_displayed = false;
        }
    }

    pub(super) fn finish_stream_line(&mut self) -> Result<()> {
        if self.stream.current().is_empty() {
            return Ok(());
        }
        self.print_stream_fragment("\n")
    }

    /// Seal streamed content when readline commits a user line. A waiting row
    /// is transient, so delete it and compact the already-rendered user line.
    pub(super) fn abandon_live_render(&mut self, input_line: Option<&str>) -> Result<()> {
        if self.waiting_dots.take().is_some() && self.live_row_displayed {
            self.printer
                .print(delete_waiting_after_input(input_line.unwrap_or_default()))?;
        }
        self.stream.clear();
        self.live_row_displayed = false;
        Ok(())
    }

    pub(super) fn start_waiting(&mut self) -> Result<()> {
        if self.waiting_dots.is_some() {
            return Ok(());
        }
        self.stream.clear();
        self.waiting_dots = Some(1);
        self.printer
            .print(live_row_message(&waiting_text(1), self.live_row_displayed))?;
        self.live_row_displayed = true;
        Ok(())
    }

    pub(super) fn advance_waiting(&mut self) -> Result<()> {
        let Some(current) = self.waiting_dots else {
            return Ok(());
        };
        let next = current % 3 + 1;
        self.waiting_dots = Some(next);
        self.printer
            .print(live_row_message(&waiting_text(next), true))?;
        Ok(())
    }

    pub(super) fn clear_waiting(&mut self) -> Result<()> {
        if self.waiting_dots.take().is_some() && self.live_row_displayed {
            self.printer
                .print(format!("{MOVE_UP_ONE}{CLEAR_LINE}\r\n"))?;
        }
        self.live_row_displayed = false;
        Ok(())
    }
}

struct ChatPrompt {
    input: UnboundedSender<LineInput>,
    announced: Cell<bool>,
}

impl ChatPrompt {
    fn new(input: UnboundedSender<LineInput>) -> Self {
        Self {
            input,
            announced: Cell::new(false),
        }
    }
}

impl Prompt for ChatPrompt {
    fn raw(&self) -> &str {
        if !self.announced.replace(true) {
            let _ = self.input.send(LineInput::PromptReady);
        }
        "> "
    }
}

#[derive(Default)]
struct LiveStream {
    current: String,
    column: usize,
}

impl LiveStream {
    fn current(&self) -> &str {
        &self.current
    }

    fn clear(&mut self) {
        self.current.clear();
        self.column = 0;
    }

    fn push(&mut self, fragment: &str, columns: usize) -> Vec<String> {
        let mut completed = Vec::new();
        let bytes = fragment.as_bytes();
        let mut index = 0;

        while index < bytes.len() {
            if bytes[index] == b'\x1b' {
                let end = ansi_sequence_end(bytes, index);
                self.current.push_str(&fragment[index..end]);
                index = end;
                continue;
            }

            let Some(character) = fragment[index..].chars().next() else {
                break;
            };
            index += character.len_utf8();

            match character {
                '\r' => {
                    if bytes.get(index) != Some(&b'\n') {
                        self.commit(&mut completed);
                    }
                }
                '\n' => self.commit(&mut completed),
                '\t' => {
                    let spaces = TAB_STOP - self.column % TAB_STOP;
                    for _ in 0..spaces {
                        self.push_character(' ', 1, columns, &mut completed);
                    }
                }
                character if !character.is_control() => {
                    let width = UnicodeWidthChar::width(character).unwrap_or(0);
                    self.push_character(character, width, columns, &mut completed);
                }
                _ => {}
            }
        }

        completed
    }

    fn push_character(
        &mut self,
        character: char,
        width: usize,
        columns: usize,
        completed: &mut Vec<String>,
    ) {
        if width > 0 && self.column > 0 && self.column.saturating_add(width) > columns {
            self.commit(completed);
        }
        self.current.push(character);
        self.column = self.column.saturating_add(width);
    }

    fn commit(&mut self, completed: &mut Vec<String>) {
        completed.push(std::mem::take(&mut self.current));
        self.column = 0;
    }
}

fn ansi_sequence_end(bytes: &[u8], start: usize) -> usize {
    let Some(next) = start.checked_add(1) else {
        return bytes.len();
    };
    if bytes.get(next) != Some(&b'[') {
        return next;
    }

    let mut end = next + 1;
    while let Some(byte) = bytes.get(end) {
        end += 1;
        if (0x40..=0x7e).contains(byte) {
            break;
        }
    }
    end
}

fn render_columns() -> usize {
    terminal_columns().saturating_sub(1).max(1)
}

fn terminal_columns() -> usize {
    terminal_size()
        .map(|(Width(columns), _)| usize::from(columns))
        .unwrap_or(FALLBACK_COLUMNS)
        .max(1)
}

fn delete_waiting_after_input(input_line: &str) -> String {
    let input_rows = input_rows(input_line, terminal_columns());
    let up = input_rows.saturating_add(1);
    format!("\u{1b}[{up}A\r\u{1b}[M\u{1b}[{input_rows}B\r")
}

fn input_rows(input_line: &str, columns: usize) -> usize {
    let mut rows = 0usize;
    let mut column = PROMPT_COLUMNS;

    for character in input_line.chars() {
        if character == '\n' {
            rows = rows.saturating_add(1);
            column = 0;
            continue;
        }
        let width = if character == '\t' {
            TAB_STOP - column % TAB_STOP
        } else {
            UnicodeWidthChar::width(character).unwrap_or(0)
        };
        column = column.saturating_add(width);
        if column > columns {
            rows = rows.saturating_add(1);
            column = width;
        }
    }

    if column == columns {
        rows = rows.saturating_add(1);
    }
    rows.saturating_add(1)
}

fn live_update_message(completed: &[String], current: &str, replace: bool) -> String {
    let completed_capacity = completed.iter().map(String::len).sum::<usize>();
    let mut message = String::with_capacity(completed_capacity + current.len() + 32);
    if replace {
        message.push_str(MOVE_UP_ONE);
        message.push_str(CLEAR_LINE);
    }
    for line in completed {
        message.push_str(line);
        message.push_str("\r\n");
    }
    if !current.is_empty() {
        message.push_str(current);
        message.push_str("\r\n");
    } else if replace && completed.is_empty() {
        message.push_str("\r\n");
    }
    message
}

fn live_row_message(content: &str, replace: bool) -> String {
    live_update_message(&[], content, replace)
}

fn waiting_text(dots: u8) -> String {
    let style = Style::new()
        .italic()
        .fg_color(Some(AnsiColor::BrightBlack.into()));
    format!("{style}waiting{}{style:#}", ".".repeat(usize::from(dots)))
}

fn normalize_newlines(message: &str) -> String {
    let mut normalized = String::with_capacity(message.len());
    let mut previous_was_cr = false;
    for character in message.chars() {
        if character == '\n' && !previous_was_cr {
            normalized.push('\r');
        }
        normalized.push(character);
        previous_was_cr = character == '\r';
    }
    normalized
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
    fn prompt_announces_readiness_once() {
        let (input, mut events) = unbounded_channel();
        let prompt = ChatPrompt::new(input);

        assert_eq!(prompt.raw(), "> ");
        assert!(matches!(events.try_recv(), Ok(LineInput::PromptReady)));
        assert_eq!(prompt.raw(), "> ");
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn streaming_rewrites_only_the_unfinished_render_row() {
        let mut stream = LiveStream::default();

        let completed = stream.push("Hel", 10);
        assert_eq!(
            live_update_message(&completed, stream.current(), false),
            "Hel\r\n"
        );
        let completed = stream.push("lo", 10);
        assert_eq!(
            live_update_message(&completed, stream.current(), true),
            "\u{1b}[A\r\u{1b}[2KHello\r\n"
        );
    }

    #[test]
    fn completed_rows_leave_only_one_live_row() {
        let mut stream = LiveStream::default();

        let completed = stream.push("abcde", 4);

        assert_eq!(completed, ["abcd"]);
        assert_eq!(stream.current(), "e");
        assert_eq!(
            live_update_message(&completed, stream.current(), false),
            "abcd\r\ne\r\n"
        );
    }

    #[test]
    fn ansi_sequences_do_not_consume_render_columns() {
        let mut stream = LiveStream::default();

        assert!(stream.push("\u{1b}[36mabc\u{1b}[0m", 3).is_empty());
        let completed = stream.push("d", 3);

        assert_eq!(completed, ["\u{1b}[36mabc\u{1b}[0m"]);
        assert_eq!(stream.current(), "d");
    }

    #[test]
    fn external_text_uses_carriage_return_line_feeds_in_raw_mode() {
        assert_eq!(
            normalize_newlines("one\ntwo\r\nthree"),
            "one\r\ntwo\r\nthree"
        );
    }

    #[test]
    fn waiting_indicator_cycles_one_to_three_dots() {
        let first = waiting_text(1);
        let second = waiting_text(2);
        let third = waiting_text(3);

        assert!(first.contains("\u{1b}[3m"));
        assert!(first.contains("\u{1b}[90m"));
        assert!(first.contains("waiting."));
        assert!(!first.contains("waiting.."));
        assert!(second.contains("waiting.."));
        assert!(third.contains("waiting..."));
        assert_eq!(
            live_row_message(&second, true),
            format!("{MOVE_UP_ONE}{CLEAR_LINE}{second}\r\n")
        );
    }

    #[test]
    fn submitted_input_deletes_the_transient_waiting_row() {
        assert_eq!(
            delete_waiting_after_input("short"),
            "\u{1b}[2A\r\u{1b}[M\u{1b}[1B\r"
        );
        assert_eq!(input_rows(&"x".repeat(79), 80), 2);
        assert_eq!(
            delete_waiting_after_input(&"x".repeat(79)),
            "\u{1b}[3A\r\u{1b}[M\u{1b}[2B\r"
        );
    }
}
