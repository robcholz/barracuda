use claw_interface::ClawFs;
use claw_memory::{MemoryDraft, MemoryPatch, Transcript, Turn, TurnId};
use serde_json::Value;
use tracing::Instrument as _;

use super::{ExtractionInput, LongTermMemoryContextProvider, MemoryOp};

/// Number of newly committed turns required for one extraction attempt, and the
/// maximum number of turns sent in that attempt.
const EXTRACTION_BATCH_TURNS: usize = 8;

struct ExtractionBatch<'a> {
    turns: Vec<&'a Turn>,
    start: TurnId,
    end: TurnId,
    skipped: usize,
}

impl<F: ClawFs + 'static> LongTermMemoryContextProvider<F> {
    /// Run one bounded extraction after a full batch of turns has committed.
    ///
    /// The cheap turn-version check avoids cloning a transcript snapshot on
    /// ordinary tool iterations. The selected batch contains only committed
    /// turns after the observed cursor, never the volatile current turn.
    pub(super) async fn maybe_extract_batch(&mut self, transcript: &dyn Transcript) {
        let turn_version = transcript.turn_version();
        let Some(previous_turn_version) = self.extraction_cursor.turn_version else {
            let turns = transcript.turns();
            self.extraction_cursor.turn_version = Some(turn_version);
            self.extraction_cursor.through = latest_committed_turn_id(&turns);
            return;
        };
        if turn_version < previous_turn_version {
            let turns = transcript.turns();
            self.extraction_cursor.turn_version = Some(turn_version);
            self.extraction_cursor.through = latest_committed_turn_id(&turns);
            return;
        }

        let turn_version_delta = turn_version.saturating_sub(previous_turn_version);
        let required_delta = u64::try_from(EXTRACTION_BATCH_TURNS).unwrap_or(u64::MAX);
        if turn_version_delta < required_delta {
            return;
        }

        let turns = transcript.turns();
        let Some(batch) = select_extraction_batch(&turns, self.extraction_cursor.through) else {
            // A version discontinuity must not turn into a hot-loop snapshot.
            self.extraction_cursor.turn_version = Some(turn_version);
            return;
        };

        // Advance before awaiting the backend. The extractor owns its transport
        // retries; after they exhaust, this optional maintenance batch is not
        // replayed on every iteration.
        self.extraction_cursor.turn_version = Some(turn_version);
        self.extraction_cursor.through = Some(batch.end);

        let transcript =
            flatten_transcript(batch.turns.iter().flat_map(|turn| turn.messages.iter()));
        if transcript.trim().is_empty() {
            return;
        }
        let existing = self.stores.snapshot();
        let input = ExtractionInput {
            transcript: &transcript,
            existing: &existing,
        };
        let span = tracing::info_span!(
            "context.extract",
            transcript_turn_version = turn_version,
            turn_version_delta,
            batch_turn_count = batch.turns.len() as u64,
            batch_start_turn = %batch.start,
            batch_end_turn = %batch.end,
            skipped_turn_count = batch.skipped as u64,
            transcript_bytes = transcript.len() as u64,
            existing_count = existing.len() as u64,
        );
        let result = self.extractor.extract(input).instrument(span.clone()).await;
        match result {
            Ok(ops) => {
                let mut add_count = 0u64;
                let mut replace_count = 0u64;
                let mut forget_count = 0u64;
                for op in &ops {
                    match op {
                        MemoryOp::Add(_) => add_count = add_count.saturating_add(1),
                        MemoryOp::Replace { .. } => {
                            replace_count = replace_count.saturating_add(1);
                        }
                        MemoryOp::Forget { .. } => {
                            forget_count = forget_count.saturating_add(1);
                        }
                    }
                }
                log::info!(
                    "long-term memory extraction completed: operations={}, \
                     added={add_count}, replaced={replace_count}, forgotten={forget_count}",
                    ops.len()
                );
                span.in_scope(|| {
                    tracing::info!(
                        name: "completed",
                        operation_count = ops.len() as u64,
                        add_count,
                        replace_count,
                        forget_count,
                    );
                });
                for op in ops {
                    self.apply_op(op);
                }
            }
            Err(error) => {
                let kind: &'static str = (&error).into();
                log::warn!("long-term memory extraction failed: kind={kind}, error={error}");
                span.in_scope(|| {
                    tracing::warn!(
                        name: "failed",
                        kind,
                        error = %error,
                    );
                });
            }
        }
    }

    fn apply_op(&self, op: MemoryOp) {
        match op {
            MemoryOp::Add(item) => {
                let draft = MemoryDraft::new(item.content)
                    .with_tags(item.tags)
                    .with_keywords(item.keywords)
                    .with_source("extracted");
                self.stores.store(draft);
            }
            MemoryOp::Replace { id, item } => {
                let patch = MemoryPatch {
                    content: Some(item.content),
                    tags: Some(item.tags),
                    keywords: Some(item.keywords),
                };
                let _ = self.stores.update(&id, patch);
            }
            MemoryOp::Forget { id } => {
                let _ = self.stores.forget(&id);
            }
        }
    }
}

fn latest_committed_turn_id(turns: &[Turn]) -> Option<TurnId> {
    turns.iter().rev().find_map(|turn| turn.id)
}

/// Select the newest bounded window of committed turns past `through`.
///
/// Taking the newest window keeps provider recreation bounded: a restored long
/// transcript does not get replayed wholesale merely because this runtime-only
/// cursor starts empty.
fn select_extraction_batch(turns: &[Turn], through: Option<TurnId>) -> Option<ExtractionBatch<'_>> {
    let mut unseen: Vec<&Turn> = turns
        .iter()
        .filter(|turn| {
            turn.id
                .is_some_and(|id| through.is_none_or(|cursor| id > cursor))
        })
        .collect();
    let skipped = unseen.len().saturating_sub(EXTRACTION_BATCH_TURNS);
    if skipped > 0 {
        unseen.drain(..skipped);
    }
    let start = unseen.first().and_then(|turn| turn.id)?;
    let end = unseen.last().and_then(|turn| turn.id)?;
    Some(ExtractionBatch {
        turns: unseen,
        start,
        end,
        skipped,
    })
}

/// Flatten a sequence of chat messages into the role-prefixed plain text an
/// extractor reads. Messages without string content (e.g. an assistant turn
/// carrying only `tool_calls`) are skipped.
fn flatten_transcript<'a>(messages: impl Iterator<Item = &'a Value>) -> String {
    let mut out = String::new();
    for message in messages {
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        let Some(content) = message.get("content").and_then(Value::as_str) else {
            continue;
        };
        if role.is_empty() || content.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(role);
        out.push_str(": ");
        out.push_str(content);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, MutexGuard};

    use claw_interface::MemFs;
    use claw_memory::{LongTermMemory, TranscriptStore};
    use futures_lite::future::block_on;

    use crate::agent::base_agent::ContextProvider;

    use super::super::extraction::{
        ExtractError, ExtractFuture, ExtractionInput, Extractor, MemoryOp,
    };
    use super::super::LongTermMemoryContextProvider;

    struct RecordingExtractor {
        calls: Mutex<Vec<String>>,
        results: Mutex<VecDeque<Result<Vec<MemoryOp>, ExtractError>>>,
    }

    impl RecordingExtractor {
        fn new(results: impl IntoIterator<Item = Result<Vec<MemoryOp>, ExtractError>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                results: Mutex::new(results.into_iter().collect()),
            }
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl Extractor for RecordingExtractor {
        fn extract<'a>(&'a self, input: ExtractionInput<'a>) -> ExtractFuture<'a> {
            lock(&self.calls).push(input.transcript.to_owned());
            let result = lock(&self.results)
                .pop_front()
                .expect("each expected extraction has a scripted result");
            Box::pin(async move { result })
        }
    }

    #[test]
    fn extraction_waits_for_eight_committed_turns_and_ignores_the_open_turn() {
        let recorder = Arc::new(RecordingExtractor::new([Ok(Vec::new())]));
        let mut provider = provider(recorder.clone());
        let transcript = transcript();
        block_on(provider.maybe_extract_batch(&transcript));

        for sequence in 0..7 {
            commit_turn(&transcript, &format!("committed-{sequence:02}"));
            block_on(provider.maybe_extract_batch(&transcript));
        }
        assert!(recorder.calls().is_empty());

        let eighth = transcript.open_turn().expect("the eighth test turn opens");
        {
            let mut user = eighth.user().expect("the eighth user message opens");
            user.append("committed-07");
        }
        block_on(provider.maybe_extract_batch(&transcript));
        assert!(recorder.calls().is_empty());
        drop(eighth);

        let current = transcript.open_turn().expect("the current test turn opens");
        {
            let mut user = current.user().expect("the current user message opens");
            user.append("open-current");
        }
        block_on(provider.maybe_extract_batch(&transcript));

        let calls = recorder.calls();
        let input = calls.first().expect("one extraction ran");
        assert_eq!(calls.len(), 1);
        assert!(input.contains("user: committed-00"));
        assert!(input.contains("user: committed-07"));
        assert!(!input.contains("open-current"));
    }

    #[test]
    fn extraction_uses_only_the_latest_bounded_incremental_batch() {
        let recorder = Arc::new(RecordingExtractor::new([Ok(Vec::new()), Ok(Vec::new())]));
        let mut provider = provider(recorder.clone());
        let transcript = transcript();

        for sequence in 0..12 {
            commit_turn(&transcript, &format!("message-{sequence:02}"));
        }
        block_on(provider.maybe_extract_batch(&transcript));
        assert!(recorder.calls().is_empty());

        for sequence in 12..24 {
            commit_turn(&transcript, &format!("message-{sequence:02}"));
        }
        block_on(provider.maybe_extract_batch(&transcript));

        for sequence in 24..32 {
            commit_turn(&transcript, &format!("message-{sequence:02}"));
        }
        block_on(provider.maybe_extract_batch(&transcript));

        let calls = recorder.calls();
        let bounded_batch = calls.first().expect("the bounded batch ran");
        let incremental_batch = calls.get(1).expect("the incremental batch ran");
        assert_eq!(calls.len(), 2);
        assert!(!bounded_batch.contains("user: message-15"));
        assert!(bounded_batch.contains("user: message-16"));
        assert!(bounded_batch.contains("user: message-23"));
        assert!(!incremental_batch.contains("user: message-23"));
        assert!(incremental_batch.contains("user: message-24"));
        assert!(incremental_batch.contains("user: message-31"));
    }

    #[test]
    fn failed_extraction_is_logged_and_not_retried_on_every_iteration() {
        let recorder = Arc::new(RecordingExtractor::new([
            Err(ExtractError::EmptyOutput),
            Ok(Vec::new()),
        ]));
        let mut provider = provider(recorder.clone());
        let transcript = transcript();
        assert!(block_on(provider.prepare(&transcript)).is_ok());

        for sequence in 0..8 {
            commit_turn(&transcript, &format!("first-{sequence:02}"));
        }
        assert!(block_on(provider.prepare(&transcript)).is_ok());
        for _ in 0..4 {
            assert!(block_on(provider.prepare(&transcript)).is_ok());
        }
        assert_eq!(recorder.calls().len(), 1);

        for sequence in 0..8 {
            commit_turn(&transcript, &format!("second-{sequence:02}"));
            assert!(block_on(provider.prepare(&transcript)).is_ok());
        }

        let calls = recorder.calls();
        let retry = calls.get(1).expect("new turns scheduled a fresh batch");
        assert_eq!(calls.len(), 2);
        assert!(!retry.contains("user: first-07"));
        assert!(retry.contains("user: second-00"));
        assert!(retry.contains("user: second-07"));
    }

    fn provider(extractor: Arc<RecordingExtractor>) -> LongTermMemoryContextProvider<MemFs> {
        let filesystem = Arc::new(MemFs::new());
        let agent = LongTermMemory::new(Arc::clone(&filesystem), "/memory/agent", "a-")
            .expect("the agent memory store opens");
        let global = LongTermMemory::new(filesystem, "/memory/global", "g-")
            .expect("the global memory store opens");
        let extractor: Arc<dyn Extractor> = extractor;
        LongTermMemoryContextProvider::new(agent, global, extractor)
    }

    fn transcript() -> TranscriptStore<MemFs> {
        TranscriptStore::new(Arc::new(MemFs::new()), 1, "/transcript")
            .expect("the transcript store opens")
    }

    fn commit_turn(transcript: &TranscriptStore<MemFs>, content: &str) {
        let turn = transcript.open_turn().expect("the test turn opens");
        {
            let mut user = turn.user().expect("the user message opens");
            user.append(content);
        }
        drop(turn);
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
