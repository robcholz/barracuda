//! Profile context provider: project editable profile documents into context.
//!
//! The store lives in `barracuda-agent-memory`; this provider is the agent-runtime layer that
//! maps documents to `BlockKind`s and exposes profile-specific tools. Per-agent
//! read/write projection is owned by the baked tool blacklist.

use barracuda_agent_context::{Block, BlockKind, ContextSink};
use barracuda_agent_memory::{ProfileDocument, ProfileError, ProfileStore};
use barracuda_agent_tool::ToolGroup;
use barracuda_fs::FileSystem;

use crate::agent::base_agent::{ContextProvider, ContextProviderResult};

use self::tools::profile_tools;

mod tools;

/// Failure while projecting profile documents into model context.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProfileProviderError {
    /// Reading one profile document failed.
    #[error(transparent)]
    Read(#[from] ProfileError),
}

/// Pulls global profile documents into the current agent context.
pub(crate) struct ProfileContextProvider<F: FileSystem + 'static> {
    store: ProfileStore<F>,
}

impl<F: FileSystem + 'static> ProfileContextProvider<F> {
    /// Build an provider over `store`.
    pub(crate) fn new(store: ProfileStore<F>) -> Self {
        Self { store }
    }

    fn contribute_document(
        &self,
        document: ProfileDocument,
        output: &mut ContextSink<'_>,
    ) -> Result<(), ProfileProviderError> {
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
                return Err(error.into());
            }
        }
        Ok(())
    }
}

impl<F: FileSystem + 'static> ContextProvider for ProfileContextProvider<F> {
    fn contribute(&mut self, output: &mut ContextSink<'_>) -> ContextProviderResult {
        for document in ProfileDocument::all() {
            self.contribute_document(document, output).map_err(
                |error| -> Box<dyn core::error::Error + Send + Sync> { Box::new(error) },
            )?;
        }
        Ok(())
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(profile_tools(self.store.clone()))
    }
}
use alloc::boxed::Box;
