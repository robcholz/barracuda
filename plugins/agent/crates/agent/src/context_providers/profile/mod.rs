//! Profile context provider: project editable profile documents into context.
//!
//! The store lives in `barracuda-agent-memory`; this provider is the agent layer that
//! maps documents to `BlockKind`s and exposes profile-specific tools. Per-agent
//! read/write projection is owned by the baked tool blacklist.

use alloc::boxed::Box;

use crate::engine::AgentStorage;
use barracuda_agent_context::{Block, BlockKind, ContextSink};
use barracuda_agent_memory::{ProfileDocument, ProfileError, ProfileSnapshot, ProfileStore};
use barracuda_agent_tool::{Tool, ToolGroup};

use crate::engine::{ContextProvider, ContextProviderResult};

use self::tools::{ProfileClearTool, ProfileReadTool, ProfileReplaceTool};

mod tools;

/// Failure while projecting profile documents into model context.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProfileProviderError {
    /// Reading one profile document failed.
    #[error(transparent)]
    Read(#[from] ProfileError),
}

/// Pulls global profile documents into the current agent context.
pub(crate) struct ProfileContextProvider {
    store: ProfileStore,
    snapshot: ProfileSnapshot,
}

impl ProfileContextProvider {
    /// Build an provider over `store`.
    pub(crate) fn new(store: ProfileStore) -> Self {
        Self {
            store,
            snapshot: ProfileSnapshot::default(),
        }
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
        let content = match document {
            ProfileDocument::Soul => self.snapshot.soul.as_deref(),
            ProfileDocument::AssistantIdentity => self.snapshot.assistant_identity.as_deref(),
            ProfileDocument::UserProfile => self.snapshot.user_profile.as_deref(),
        };
        match content {
            Some(content) => {
                output.block(Block::new(kind, content));
            }
            None => {
                output.block(Block::new(kind, ""));
            }
        }
        Ok(())
    }
}

impl ContextProvider for ProfileContextProvider {
    fn id(&self) -> &'static str {
        "profile"
    }

    fn prepare<'a>(
        &'a mut self,
        _storage: &'a AgentStorage,
        _transcript: &'a dyn barracuda_agent_memory::Transcript,
    ) -> crate::engine::ContextProviderFuture<'a> {
        Box::pin(async move {
            self.snapshot = self.store.snapshot().await.map_err(
                |error| -> Box<dyn core::error::Error + Send + Sync> { Box::new(error) },
            )?;
            Ok(())
        })
    }

    fn contribute(
        &mut self,
        _storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        for document in ProfileDocument::all() {
            self.contribute_document(document, output).map_err(
                |error| -> Box<dyn core::error::Error + Send + Sync> { Box::new(error) },
            )?;
        }
        Ok(())
    }

    fn tools(&self, _storage: &AgentStorage) -> Option<ToolGroup> {
        Some(ToolGroup::new(
            self.id(),
            true,
            [
                Tool::new(ProfileReadTool {
                    store: self.store.clone(),
                }),
                Tool::new(ProfileReplaceTool {
                    store: self.store.clone(),
                }),
                Tool::new(ProfileClearTool {
                    store: self.store.clone(),
                }),
            ],
        ))
    }
}
