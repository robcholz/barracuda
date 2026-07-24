//! Profile context adapter: project editable profile documents into context.
//!
//! The store lives in `claw-memory`; this adapter is the agent-runtime layer that
//! maps documents to `BlockKind`s and exposes profile-specific tools. Per-agent
//! read/write projection is owned by the baked tool blacklist.

use claw_context::{Block, BlockKind, ContextSink};
use claw_interface::ClawFs;
use claw_memory::{ProfileDocument, ProfileStore};
use claw_tool::ToolGroup;

use crate::agent::base_agent::ContextAdapter;

use self::tools::profile_tools;

mod tools;

/// Pulls global profile documents into the current agent context.
pub(crate) struct ProfileContextAdapter<F: ClawFs + 'static> {
    store: ProfileStore<F>,
}

impl<F: ClawFs + 'static> ProfileContextAdapter<F> {
    /// Build an adapter over `store`.
    pub(crate) fn new(store: ProfileStore<F>) -> Self {
        Self { store }
    }

    fn contribute_document(&self, document: ProfileDocument, output: &mut ContextSink<'_>) {
        let kind = match document {
            ProfileDocument::Soul => BlockKind::Soul,
            ProfileDocument::AssistantIdentity => BlockKind::AssistantIdentity,
            ProfileDocument::UserProfile => BlockKind::UserProfile,
        };
        match self.store.read(document) {
            Ok(Some(content)) => {
                output.block(Block::new(kind, content));
            }
            Ok(None) => {
                output.block(Block::new(kind, ""));
            }
            Err(error) => {
                log::warn!("profile context read failed for {document}: {error}");
                tracing::warn!(
                    name: "profile_context_read_failed",
                    document = %document,
                    error = %error,
                );
            }
        }
    }
}

impl<F: ClawFs + 'static> ContextAdapter for ProfileContextAdapter<F> {
    fn contribute(&mut self, output: &mut ContextSink<'_>) {
        for document in ProfileDocument::all() {
            self.contribute_document(document, output);
        }
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(profile_tools(self.store.clone()))
    }
}
