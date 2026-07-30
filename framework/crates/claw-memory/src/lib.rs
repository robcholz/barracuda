//! `claw-memory` — the agent memory subsystem.
//!
//! Two stores live here, both pure storage:
//!
//! - [`TranscriptStore`] — the complete, append-only verbatim transcript
//!   (the source of truth for what was said).
//! - [`ProfileStore`] — the editable global profile documents (`soul.md`,
//!   `identity.md`, `user.md`).
//! - [`LongTermMemory`] — the durable fact store.
//!
//! As a core crate it depends only on the [`claw_interface`] inbound traits — the
//! [`ClawFs`](claw_interface::ClawFs) persistence seam — never on the platform
//! boundary (`claw-sys`) or on the LLM client (`claw-api`).
//!
//! # Compaction is *not* here
//!
//! Folding an aged transcript prefix into a summary so it fits the model's
//! context window is a property of the **LLM request**, not of the stored record.
//! The [`TranscriptStore`] therefore never summarizes or deletes turns; it just
//! stores them. This crate only defines the [`Compactor`] *seam* — the
//! transformation "turn a window of messages into a summary" — which the agent
//! layer's rolling-summary context provider (in `claw_core`) owns and drives. The
//! ready-made LLM-backed compactor (`LlmCompactor`) lives in `claw_core`, which
//! has the LLM client.
//!
//! # Using the transcript store
//!
//! ```no_run
//! use claw_interface::MemFs;
//! use claw_memory::{AssistantFragment, TranscriptStore};
//! use std::sync::Arc;
//!
//! // A filesystem for persistence. On device this is the espidf `ClawFs` over
//! // the DATA root; here it is the in-memory host double.
//! let filesystem = Arc::new(MemFs::new());
//!
//! // Build the store for one transcript id. Typically one per agent instance.
//! let transcript_id = 42;
//! let store = TranscriptStore::new(filesystem, transcript_id, "/data/transcripts")
//!     .expect("a fresh MemFs has no data log, so the transcript starts empty");
//!
//! // Child handles finish messages; the turn handle commits the record.
//! let turn = store.open_turn().expect("the store has no active turn");
//! {
//!     let mut user = turn.user().unwrap();
//!     user.append("what's the weather?");
//! }
//! {
//!     let mut assistant = turn.assistant().unwrap();
//!     assistant.append(AssistantFragment::Content("Sun"));
//!     assistant.append(AssistantFragment::Content("ny."));
//! }
//!
//! // turns() includes the open turn (id == None) as the trailing element;
//! // the flat model-facing transcript is its messages flattened.
//! let turns = store.turns();
//! let _messages: Vec<_> = turns.iter().flat_map(|t| &t.messages).collect();
//! drop(turn); // commit + persist
//!
//! // Persistence is automatic at the turn boundary.
//! ```

pub mod compaction;
pub mod long_term_memory;
pub mod profile;
pub mod transcript_store;

#[cfg(feature = "compactor-stub")]
pub use compaction::NoopCompactor;
pub use compaction::{CompactError, CompactFuture, Compactor};
pub use long_term_memory::{
    LongTermError, LongTermInitError, LongTermMemory, MemoryDraft, MemoryId, MemoryItem,
    MemoryPatch, StoreOutcome,
};
pub use profile::{
    ParseProfileDocumentError, ProfileDocument, ProfileError, ProfileSnapshot, ProfileStore,
    ASSISTANT_IDENTITY_FILE, DEFAULT_PROFILE_DOCUMENT_MAX_BYTES, SOUL_FILE, USER_PROFILE_FILE,
};
pub use transcript_store::{
    AssistantFragment, AssistantHandle, ToolHandle, Transcript, TranscriptDeleteError,
    TranscriptInitError, TranscriptListError, TranscriptStore, Turn, TurnError, TurnHandle, TurnId,
    UserHandle,
};
