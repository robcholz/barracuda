//! Terminal WebSocket client: an external IM endpoint for the host server.
//!
//! Sends user lines as gateway messages and renders the reply stream. Rich
//! content arrives as ordinary IM messages tagged with a [`gateway::MessageKind`]
//! role, rendered distinctly in the terminal.

use anstyle::{AnsiColor, Style};
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::io::IsTerminal;
use tokio::time::{interval, sleep, Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

use crate::line_editor::{ChatLineEditor, LineInput};
use crate::protocol::parse_sse;
use web::WebClientFrame;

const WAITING_TICK: Duration = Duration::from_millis(400);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const LOCAL_CONNECT_RETRY: Duration = Duration::from_millis(10);

/// Connects to the host server at `url` and runs the terminal chat loop.
pub async fn run(url: &str) -> Result<()> {
    let (websocket, _response) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| connection_error(url, error))?;
    run_connected(url, websocket).await
}

/// Connects to a System started concurrently by this process.
pub(crate) async fn run_when_available(url: &str) -> Result<()> {
    let deadline = Instant::now() + LOCAL_CONNECT_TIMEOUT;
    loop {
        match tokio_tungstenite::connect_async(url).await {
            Ok((websocket, _response)) => return run_connected(url, websocket).await,
            Err(_error) if Instant::now() < deadline => sleep(LOCAL_CONNECT_RETRY).await,
            Err(error) => return Err(connection_error(url, error)),
        }
    }
}

fn connection_error(url: &str, error: tokio_tungstenite::tungstenite::Error) -> anyhow::Error {
    anyhow!("failed to connect to {url}: {error}")
}

async fn run_connected(
    url: &str,
    websocket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<()> {
    let (mut sink, mut source) = websocket.split();

    eprintln!("Connected to {url}. Type a message, or /quit to exit.\n");

    let mut editor = ChatLineEditor::new()?;
    let mut renderer = Renderer::default();
    let mut waiting = false;
    let mut ticker = interval(WAITING_TICK);
    editor.show_prompt().await?;

    loop {
        tokio::select! {
            input = editor.next_input() => match input {
                Some(LineInput::Line(line)) => {
                    editor.abandon_live_render(Some(&line))?;
                    let trimmed = line.trim();
                    if trimmed == "/quit" || trimmed == "/exit" {
                        break;
                    }
                    if !trimmed.is_empty() {
                        let frame = serde_json::to_string(&WebClientFrame {
                            text: trimmed.to_string(),
                            reply_to: None,
                        })?;
                        sink.send(Message::text(frame)).await?;
                        editor.start_waiting()?;
                        waiting = true;
                        ticker.reset();
                    }
                    editor.show_prompt().await?;
                }
                Some(LineInput::Interrupted) | Some(LineInput::Eof) | None => break,
                Some(LineInput::PromptReady) => {}
                Some(LineInput::Failed(error)) => return Err(error.into()),
            },
            incoming = source.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if waiting {
                        editor.clear_waiting()?;
                        waiting = false;
                    }
                    for action in renderer.absorb(text.as_str()) {
                        match action {
                            RenderAction::Start(kind) => {
                                editor.finish_stream();
                                if let Some(prefix) = kind.stream_prefix() {
                                    editor.print_stream_fragment(&prefix)?;
                                }
                            }
                            RenderAction::Delta(delta) => {
                                editor.print_stream_fragment(&delta)?;
                            }
                            RenderAction::Print(message) => {
                                editor.finish_stream_line()?;
                                editor.print(message)?;
                            }
                            RenderAction::End => editor.finish_stream_line()?,
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    editor.print("Connection closed.".to_string())?;
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
            },
            _ = ticker.tick() => {
                if waiting {
                    editor.advance_waiting()?;
                }
            }
        }
    }

    editor.print("Goodbye.".to_string())?;
    Ok(())
}

/// Maps the Web protocol's message lifecycle onto terminal rendering actions.
#[derive(Default)]
struct Renderer {
    extra: Option<MessageKind>,
}

#[derive(Debug, Eq, PartialEq)]
enum RenderAction {
    Start(MessageKind),
    Delta(String),
    Print(String),
    End,
}

impl Renderer {
    fn absorb(&mut self, frame: &str) -> Vec<RenderAction> {
        let mut actions = Vec::new();
        let Some(frame) = parse_sse(frame) else {
            return actions;
        };
        let data: serde_json::Value = serde_json::from_str(&frame.data).unwrap_or_default();
        match frame.event.as_str() {
            "message.start" => {
                self.extra = None;
                actions.push(RenderAction::Start(MessageKind::from_data(&data)));
            }
            "message.delta" => {
                if self.extra.take().is_some() {
                    actions.push(RenderAction::End);
                }
                if let Some(delta) = data
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .filter(|delta| !delta.is_empty())
                {
                    actions.push(RenderAction::Delta(delta.to_string()));
                }
            }
            "message.extra"
                if data.get("field").and_then(serde_json::Value::as_str) == Some("notice") =>
            {
                if let Some(notice) = data
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .filter(|notice| !notice.is_empty())
                {
                    actions.push(RenderAction::Print(MessageKind::Notice.render(notice)));
                }
            }
            "message.extra" => {
                let kind = match data.get("field").and_then(serde_json::Value::as_str) {
                    Some("reasoning") => Some(MessageKind::Reasoning),
                    Some("effect_result") => Some(MessageKind::Reply),
                    Some("tool_output") => Some(MessageKind::Tool),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let content = data
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let complete = data.get("boundary").and_then(serde_json::Value::as_str)
                        == Some("complete");
                    self.stream_extra(kind, content, complete, &mut actions);
                }
            }
            "message.end" => {
                if self.extra.take().is_some() {
                    actions.push(RenderAction::End);
                }
                actions.push(RenderAction::End);
                if let Some(error) = data
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .filter(|error| !error.is_empty())
                {
                    actions.push(RenderAction::Print(MessageKind::Notice.render(error)));
                }
            }
            _ => {}
        }
        actions
    }

    fn stream_extra(
        &mut self,
        kind: MessageKind,
        content: &str,
        complete: bool,
        actions: &mut Vec<RenderAction>,
    ) {
        if self.extra != Some(kind) {
            if self.extra.replace(kind).is_some() {
                actions.push(RenderAction::End);
            }
            actions.push(RenderAction::Start(kind));
        }
        if !content.is_empty() {
            actions.push(RenderAction::Delta(content.to_string()));
        }
        if complete {
            self.extra = None;
            actions.push(RenderAction::End);
        }
    }
}

/// Presentation role mirrored from the gateway `kind`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MessageKind {
    #[default]
    Reply,
    Reasoning,
    Tool,
    Notice,
}

impl MessageKind {
    fn from_data(data: &serde_json::Value) -> Self {
        match data.get("kind").and_then(serde_json::Value::as_str) {
            Some("reasoning") => Self::Reasoning,
            Some("tool") => Self::Tool,
            Some("notice") => Self::Notice,
            _ => Self::Reply,
        }
    }

    fn render(self, text: &str) -> String {
        match self {
            Self::Reply => text.to_string(),
            Self::Reasoning => label("think", text, self.style()),
            Self::Tool => label("tool", text, self.style()),
            Self::Notice => label("note", text, self.style()),
        }
    }

    fn stream_prefix(self) -> Option<String> {
        let tag = match self {
            Self::Reply => return None,
            Self::Reasoning => "think",
            Self::Tool => "tool",
            Self::Notice => "note",
        };
        let style = self.style();
        Some(format!("  {style}{tag:<5}{style:#}  "))
    }

    fn style(self) -> Style {
        if !std::io::stderr().is_terminal() {
            return Style::new();
        }
        match self {
            Self::Reply => Style::new(),
            Self::Reasoning => Style::new().dimmed().fg_color(Some(AnsiColor::Cyan.into())),
            Self::Tool => Style::new().bold().fg_color(Some(AnsiColor::Green.into())),
            Self::Notice => Style::new()
                .dimmed()
                .fg_color(Some(AnsiColor::Yellow.into())),
        }
    }
}

fn label(tag: &str, text: &str, style: Style) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut rendered = format!("  {style}{tag:<5}{style:#}  {first}");
    for line in lines {
        rendered.push_str("\n         ");
        rendered.push_str(line);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_actions(frames: &[&str]) -> Vec<RenderAction> {
        let mut renderer = Renderer::default();
        frames
            .iter()
            .flat_map(|frame| renderer.absorb(frame))
            .collect()
    }

    #[test]
    fn kind_maps_from_gateway_role() {
        let reasoning = serde_json::json!({ "kind": "reasoning" });
        assert!(matches!(
            MessageKind::from_data(&reasoning),
            MessageKind::Reasoning
        ));
        let tool = serde_json::json!({ "kind": "tool" });
        assert!(matches!(MessageKind::from_data(&tool), MessageKind::Tool));
        let notice = serde_json::json!({ "kind": "notice" });
        assert!(matches!(
            MessageKind::from_data(&notice),
            MessageKind::Notice
        ));
        // Missing or unknown roles fall back to a plain reply.
        assert!(matches!(
            MessageKind::from_data(&serde_json::json!({})),
            MessageKind::Reply
        ));
    }

    #[test]
    fn reply_renders_as_plain_text_and_others_are_labeled() {
        assert_eq!(MessageKind::Reply.render("hello"), "hello");
        assert!(MessageKind::Tool.render("search: ok").contains("tool"));
        assert!(MessageKind::Reasoning.render("thinking").contains("think"));
    }

    #[test]
    fn label_indents_continuation_lines() {
        let rendered = label("note", "line one\nline two", Style::new());
        assert!(rendered.contains("note"));
        assert!(rendered.contains("\n         line two"));
    }

    #[test]
    fn notice_extra_is_rendered() {
        let actions = render_actions(&[
            "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
            "event: message.extra\ndata: {\"field\":\"notice\",\"boundary\":\"complete\",\"content\":\"failed to initialize LLM chat: request timed out\"}\n\n",
            "event: message.end\ndata: {\"error\":null}\n\n",
        ]);

        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], RenderAction::Start(MessageKind::Reply));
        assert!(
            matches!(&actions[1], RenderAction::Print(text) if text.contains("note") && text.contains("request timed out"))
        );
        assert_eq!(actions[2], RenderAction::End);
    }

    #[test]
    fn message_end_error_is_rendered() {
        let actions = render_actions(&[
            "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
            "event: message.end\ndata: {\"error\":\"gateway response stream failed\"}\n\n",
        ]);

        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], RenderAction::Start(MessageKind::Reply));
        assert_eq!(actions[1], RenderAction::End);
        assert!(
            matches!(&actions[2], RenderAction::Print(text) if text.contains("note") && text.contains("gateway response stream failed"))
        );
    }

    #[test]
    fn web_message_deltas_remain_separate_streaming_actions() {
        let actions = render_actions(&[
            "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
            "event: message.delta\ndata: {\"delta\":\"hel\"}\n\n",
            "event: message.delta\ndata: {\"delta\":\"lo\"}\n\n",
            "event: message.end\ndata: {\"error\":null}\n\n",
        ]);

        assert_eq!(
            actions,
            vec![
                RenderAction::Start(MessageKind::Reply),
                RenderAction::Delta("hel".to_string()),
                RenderAction::Delta("lo".to_string()),
                RenderAction::End,
            ]
        );
    }

    #[test]
    fn web_reasoning_extras_follow_stream_boundaries() {
        let actions = render_actions(&[
            "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
            "event: message.extra\ndata: {\"field\":\"reasoning\",\"boundary\":\"more\",\"content\":\"think\"}\n\n",
            "event: message.extra\ndata: {\"field\":\"reasoning\",\"boundary\":\"complete\",\"content\":\"ing\"}\n\n",
            "event: message.end\ndata: {\"error\":null}\n\n",
        ]);

        assert_eq!(
            actions,
            vec![
                RenderAction::Start(MessageKind::Reply),
                RenderAction::Start(MessageKind::Reasoning),
                RenderAction::Delta("think".to_string()),
                RenderAction::Delta("ing".to_string()),
                RenderAction::End,
                RenderAction::End,
            ]
        );
    }
}
