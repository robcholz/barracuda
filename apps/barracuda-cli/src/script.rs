//! Non-interactive terminal Channel for automation and end-to-end tests.
//!
//! Reads one message per stdin line, sends each after the previous reply has
//! settled, and writes every completed message as one JSON object per stdout
//! line: `{"turn":0,"kind":"reply","text":"..."}`.

use std::io::{BufRead as _, Write as _};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::tungstenite::Message;

use crate::client::{MessageKind, RenderAction, Renderer};
use barracuda_imessage_web_plugin::WebClientFrame;

/// Timing policy for deciding when a reply turn is complete.
#[derive(Clone, Copy, Debug)]
pub struct ScriptTiming {
    /// Longest wait for one turn before the run fails.
    pub turn: Duration,
    /// Quiet period after the last completed message that ends a turn.
    pub settle: Duration,
}

impl Default for ScriptTiming {
    fn default() -> Self {
        Self {
            turn: Duration::from_secs(180),
            settle: Duration::from_secs(3),
        }
    }
}

/// Sends stdin lines to the System at `url` and prints completed messages.
pub async fn run(url: &str, timing: ScriptTiming) -> Result<()> {
    let (websocket, _response) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| anyhow!("failed to connect to {url}: {error}"))?;
    let (mut sink, mut source) = websocket.split();
    let lines = std::io::stdin()
        .lock()
        .lines()
        .collect::<std::io::Result<Vec<_>>>()
        .context("read stdin")?;
    let mut stdout = std::io::stdout().lock();
    for (turn, line) in lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let frame = serde_json::to_string(&WebClientFrame {
            text: line.to_string(),
            reply_to: None,
        })?;
        sink.send(Message::text(frame)).await?;
        let mut collector = TurnCollector::default();
        let deadline = Instant::now() + timing.turn;
        loop {
            let wait_until = match collector.settled_at() {
                Some(settled) => (settled + timing.settle).min(deadline),
                None => deadline,
            };
            match timeout_at(wait_until, source.next()).await {
                Err(_elapsed) if collector.settled_at().is_some() => break,
                Err(_elapsed) => bail!("turn {turn} did not complete within {:?}", timing.turn),
                Ok(Some(Ok(Message::Text(text)))) => collector.absorb(text.as_str()),
                Ok(Some(Ok(Message::Close(_))) | None) => {
                    bail!("connection closed during turn {turn}")
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => return Err(error.into()),
            }
        }
        for (kind, text) in collector
            .completed
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
        {
            let record = serde_json::json!({ "turn": turn, "kind": kind.as_str(), "text": text });
            writeln!(stdout, "{record}")?;
        }
        stdout.flush()?;
    }
    Ok(())
}

/// Folds render actions into completed messages for one turn.
#[derive(Default)]
struct TurnCollector {
    renderer: Renderer,
    open: Vec<(MessageKind, String)>,
    completed: Vec<(MessageKind, String)>,
    settled: Option<Instant>,
}

impl TurnCollector {
    fn absorb(&mut self, frame: &str) {
        for action in self.renderer.absorb(frame) {
            match action {
                RenderAction::Start(kind) => self.open.push((kind, String::new())),
                RenderAction::Delta(delta) => {
                    if let Some((_kind, text)) = self.open.last_mut() {
                        text.push_str(&delta);
                    }
                }
                RenderAction::Print(text) => {
                    self.completed
                        .push((MessageKind::Notice, strip_label(&text)));
                }
                RenderAction::End => {
                    if let Some(message) = self.open.pop() {
                        self.completed.push(message);
                    }
                }
            }
        }
        self.settled = (self.open.is_empty() && !self.completed.is_empty()).then(Instant::now);
    }

    fn settled_at(&self) -> Option<Instant> {
        self.settled
    }
}

/// Removes the terminal label the renderer adds to printed notices.
fn strip_label(text: &str) -> String {
    let text = text.trim_start();
    text.strip_prefix("note")
        .map_or(text, str::trim_start)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::TurnCollector;

    #[test]
    fn collects_nested_messages_and_settles_after_the_outer_end() {
        let mut collector = TurnCollector::default();
        for frame in [
            "event: message.start\ndata: {\"kind\":\"reply\"}\n\n",
            "event: message.event\ndata: {\"type\":\"reasoning_delta\",\"payload\":{\"text\":\"plan\"}}\n\n",
            "event: message.event\ndata: {\"type\":\"reasoning_ended\",\"payload\":{}}\n\n",
            "event: message.delta\ndata: {\"delta\":\"hi\"}\n\n",
        ] {
            collector.absorb(frame);
            assert!(collector.settled_at().is_none());
        }
        collector.absorb("event: message.end\ndata: {\"error\":null}\n\n");
        assert!(collector.settled_at().is_some());
        let completed: Vec<_> = collector
            .completed
            .iter()
            .map(|(kind, text)| (kind.as_str(), text.as_str()))
            .collect();
        assert_eq!(completed, [("reasoning", "plan"), ("reply", "hi")]);
    }
}
