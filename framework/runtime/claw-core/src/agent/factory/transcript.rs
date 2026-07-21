//! Concrete transcript-store binding at the filesystem-aware Factory boundary.

use std::sync::Arc;

use claw_interface::ClawFs;
use claw_memory::TranscriptStore;
use serde_json::{json, Value};

use crate::agent::base_agent::{AssistantCommit, History, Transcript};

impl<F: ClawFs + 'static> History for TranscriptStore<F> {
    fn messages(&self) -> Arc<Value> {
        TranscriptStore::messages(self)
    }

    fn version(&self) -> u64 {
        TranscriptStore::version(self)
    }
}

impl<F: ClawFs + 'static> Transcript for TranscriptStore<F> {
    fn append_user(&self, text: &str, starts_task: bool) {
        if starts_task {
            self.commit_open_turn();
        }
        self.push_user_message(text);
    }

    fn commit_assistant(&self, commit: AssistantCommit<'_>) {
        match commit {
            AssistantCommit::RawJson(raw) => self.push_assistant_message(raw),
            AssistantCommit::PlainText(text) => {
                self.push_patch(&json!([{ "role": "assistant", "content": text }]));
            }
        }
        self.commit_open_turn();
    }

    fn append_patch(&self, patch: &Value) {
        self.push_patch(patch);
    }

    fn commit_ended(&self, final_message: &str) {
        self.push_patch(&json!([{ "role": "assistant", "content": final_message }]));
        self.commit_open_turn();
    }

    fn discard_open_turn(&self) {
        TranscriptStore::discard_open_turn(self);
    }

    fn as_history(&self) -> &dyn History {
        self
    }
}
