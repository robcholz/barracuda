//! Long-term-memory extraction, context projection, and model-callable tools.
//!
//! Global and per-agent stores appear as one catalog; id prefixes route writes
//! back to the owning store.

use std::sync::Arc;

use claw_context::{Block, BlockKind, ContextSink};
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_memory::{LongTermInitError, LongTermMemory, Transcript, TurnId};
use claw_tool::ToolGroup;

use crate::agent::base_agent::{ContextProvider, ContextProviderFuture, ContextProviderResult};
use crate::config::SharedApiManager;

mod extraction;
mod extraction_flow;
mod llm_extractor;
mod stores;
mod tier;

use self::llm_extractor::LlmExtractor;
use self::stores::{agent_store, global_store, MemoryStores};
use self::tools::memory_tools;
mod tools;
use extraction::{ExtractionInput, Extractor, MemoryOp, MemorySnapshot};
use tier::MemoryTier;

type ProviderBuilder<F> =
    dyn Fn(LongTermMemory<F>, LongTermMemory<F>) -> LongTermMemoryContextProvider<F>;

/// The provider's rendered-catalog cache, keyed on each store's change version.
#[derive(Default)]
struct CatalogCache {
    global_version: u64,
    agent_version: u64,
    global_block: String,
    agent_block: String,
    /// `false` until the first render populates the blocks (version 0 is a real
    /// state, an empty store, so a flag distinguishes "never rendered").
    primed: bool,
}

/// The most recent committed transcript boundary accounted for by this provider.
///
/// Existing history becomes the initial baseline. After that this is an attempt
/// cursor, not a success cursor: the extractor already owns its backend retry
/// policy, so an exhausted batch is not retried on every LLM iteration. A later
/// batch gets a fresh attempt after enough new turns arrive.
#[derive(Default)]
struct ExtractionCursor {
    /// `None` until the provider observes its transcript for the first time.
    /// Existing history becomes the baseline instead of being re-extracted
    /// whenever an agent/provider is reconstructed.
    turn_version: Option<u64>,
    through: Option<TurnId>,
}

/// A [`ContextProvider`] over a dual-tier long-term store. See the module docs.
pub(in crate::agent) struct LongTermMemoryContextProvider<F: ClawFs + 'static> {
    stores: MemoryStores<F>,
    extractor: Arc<dyn Extractor>,
    /// Rebuilt only when a store version advances.
    catalog: CatalogCache,
    extraction_cursor: ExtractionCursor,
}

impl<F: ClawFs + 'static> LongTermMemoryContextProvider<F> {
    /// Build an provider over the two stores and an `extractor`.
    fn new(
        agent: LongTermMemory<F>,
        global: LongTermMemory<F>,
        extractor: Arc<dyn Extractor>,
    ) -> Self {
        Self {
            stores: MemoryStores { global, agent },
            extractor,
            catalog: CatalogCache::default(),
            extraction_cursor: ExtractionCursor::default(),
        }
    }

    /// Open the shared tier with the provider's canonical ID namespace.
    pub(in crate::agent) fn open_global_store(
        filesystem: Arc<F>,
        dir: &str,
    ) -> Result<LongTermMemory<F>, LongTermInitError> {
        global_store(filesystem, dir)
    }

    /// Open an Agent tier with the provider's canonical ID namespace.
    pub(in crate::agent) fn open_agent_store(
        filesystem: Arc<F>,
        dir: &str,
    ) -> Result<LongTermMemory<F>, LongTermInitError> {
        agent_store(filesystem, dir)
    }

    /// Build the shared LLM-backed provider constructor used by AgentManager.
    pub(in crate::agent) fn llm_builder<H, Timer>(
        api_manager: SharedApiManager,
    ) -> Arc<ProviderBuilder<F>>
    where
        H: ClawHttp + Default + 'static,
        Timer: ClawTimer + Default + 'static,
    {
        let extractor = LlmExtractor::<H, Timer>::shared(api_manager);
        Arc::new(move |agent, global| Self::new(agent, global, Arc::clone(&extractor)))
    }

    fn refresh_catalog(&mut self) {
        let global_version = self.stores.global.version();
        let agent_version = self.stores.agent.version();
        // Rebuild a block's text only when its store changed (or on first refresh).
        if !self.catalog.primed || self.catalog.global_version != global_version {
            self.catalog.global_block = render_catalog(
                "Shared long-term memory topics",
                &self.stores.global.catalog(),
            );
            self.catalog.global_version = global_version;
        }
        if !self.catalog.primed || self.catalog.agent_version != agent_version {
            self.catalog.agent_block =
                render_catalog("Your long-term memory topics", &self.stores.agent.catalog());
            self.catalog.agent_version = agent_version;
        }
        self.catalog.primed = true;
    }
}

impl<F: ClawFs + 'static> ContextProvider for LongTermMemoryContextProvider<F> {
    fn prepare<'a>(&'a mut self, transcript: &'a dyn Transcript) -> ContextProviderFuture<'a> {
        Box::pin(async move {
            // Pull, not push: reading the transcript here is where this provider
            // decides whether new conversation warrants extraction.
            self.maybe_extract_batch(transcript).await;
            self.refresh_catalog();
            Ok(())
        })
    }

    fn contribute(&mut self, output: &mut ContextSink<'_>) -> ContextProviderResult {
        // Borrow the cached strings into the blocks; `Context::with` copies them
        // only on a real change, so an unchanged catalog allocates nothing here.
        output.block(Block::new(
            BlockKind::GlobalMemory,
            self.catalog.global_block.as_str(),
        ));
        output.block(Block::new(
            BlockKind::AgentMemory,
            self.catalog.agent_block.as_str(),
        ));
        Ok(())
    }

    fn tools(&self) -> Option<ToolGroup> {
        Some(memory_tools(self.stores.clone()))
    }
}

/// Render a label catalog as a single durable-context line, or empty when there
/// are no labels (the context then drops the block).
fn render_catalog(header: &str, labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("{header}: {}", labels.join(", "))
    }
}
