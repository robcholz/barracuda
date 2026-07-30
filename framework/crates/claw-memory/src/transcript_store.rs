//! `TranscriptStore` — the agent's complete, append-only transcript record.
//!
//! This is the **source of truth** for what was said: every committed turn, kept
//! verbatim, forever (within the store's own lifetime). Its natural unit is a
//! **turn**: one turn's worth of messages, produced together by the agent loop:
//!
//! ```text
//! turn {                        // one group, one [`TurnId`]
//!   user message
//!   assistant message (text and/or tool_calls)
//!   tool result(s)
//!   assistant message
//! }
//! ```
//!
//! The store owns committed turns, oldest-to-newest, each stamped with a
//! monotonic [`TurnId`]. An in-progress turn belongs exclusively to its
//! [`TurnHandle`] and is exposed to readers as the trailing turn without an id.
//!
//! # It does not compact
//!
//! Compaction — summarizing an aged prefix so the transcript fits the model's
//! context window — is **not** this store's job. Compaction is a property of the
//! *LLM request*, not of the record: the summary is a derived artifact assembled
//! at request time by the agent layer's rolling-summary context provider, which
//! *reads* this store ([`turns`](TranscriptStore::turns)) and
//! keeps its own summary. This store never deletes a turn, never folds turns into
//! a summary, and has no token budget. It just stores and replays the verbatim
//! transcript. (Bounding on-disk growth, if ever needed, is a separate retention
//! concern — also not compaction.)
//!
//! # Nested RAII scopes
//!
//! The unique [`TurnHandle`] returned by [`open_turn`](TranscriptStore::open_turn)
//! owns one turn. [`UserHandle`], [`AssistantHandle`], and [`ToolHandle`] append
//! the role-specific content; dropping a child handle finishes that message.
//! Dropping the turn commits it and writes it to the injected filesystem.
//! In-progress content remains visible through [`Transcript::turns`] as the
//! trailing turn with no id.
//!
//! # Threading
//!
//! All state mutation (appends, commits, persistence) happens on the
//! **foreground** thread that owns the store. A single store must be driven from
//! one thread; the `Arc`-backed state lets a reader (a context provider) hold a
//! clone for its read snapshots.
//!
//! # Identity and persistence
//!
//! Each store is keyed by a `transcript_id`. A **persisting** store (built with
//! [`TranscriptStore::new`]) keeps two files under the `dir` it is given:
//!
//! - `<id>.jsonl` — the **data log**: one JSON record per line (`group`),
//!   **append-only**. The source of truth.
//! - `<id>.json` — the **index manifest**: a rebuildable cache listing the byte
//!   `(off, len)` of every record plus `covered_len` and `next_id`, rewritten
//!   atomically as turns are appended.
//!
//! Ephemeral transcripts use the same code path with their own [`MemFs`]
//! instance. Persistence behavior therefore comes entirely from the injected
//! filesystem instead of a separate store mode.
//!
//! [`MemFs`]: claw_interface::MemFs

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use claw_interface::{ClawFile, ClawFs, FsError};

/// Per-transcript filenames: `{dir}/{id}{DATA_EXT|INDEX_EXT}`.
const DATA_EXT: &str = ".jsonl";
const INDEX_EXT: &str = ".json";
/// Manifest schema version, so a future layout change can be detected on load.
const MANIFEST_VERSION: u32 = 1;

claw_utils::define_prefixed_id!(TurnId, "turn-", "turn");

impl TurnId {
    /// The id that immediately follows this one.
    fn next(self) -> TurnId {
        TurnId::new(self.0.saturating_add(1))
    }
}

claw_utils::define_id_allocator!(
    /// Hands out this transcript's [`TurnId`]s. Single-owner (a `StoreState`
    /// field mutated under the store lock), so the lock it needs is that outer
    /// one, not one of its own. Its position is persisted via
    /// [`peek`](TurnIdAllocator::peek) into the manifest and restored with
    /// [`starting_at`](TurnIdAllocator::starting_at).
    TurnIdAllocator(TurnId),
    TurnId::new(1)
);

/// A byte position within the data log. Addressing only — never compared for
/// chronology (that is [`TurnId`]'s job).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct ByteOffset(usize);

impl ByteOffset {
    /// The offset `len` bytes further on.
    fn advance(self, len: ByteLen) -> ByteOffset {
        ByteOffset(self.0.saturating_add(len.0))
    }
    /// As the `u64` the [`ClawFs`] read API expects (widening, lossless).
    fn as_u64(self) -> u64 {
        self.0 as u64
    }
    /// This position viewed as the length of the region `[0, self)`.
    fn as_len(self) -> ByteLen {
        ByteLen(self.0)
    }
}

/// A number of bytes: one record line, the whole data log, a covered prefix, …
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct ByteLen(usize);

impl ByteLen {
    /// The length of a serialized line.
    fn of(bytes: &[u8]) -> ByteLen {
        ByteLen(bytes.len())
    }
    /// As the `usize` the [`ClawFs`] read API expects.
    fn as_usize(self) -> usize {
        self.0
    }
    /// As the starting position just past a region of this size.
    fn as_offset(self) -> ByteOffset {
        ByteOffset(self.0)
    }
    fn saturating_sub(self, other: ByteLen) -> ByteLen {
        ByteLen(self.0.saturating_sub(other.0))
    }
    /// From a [`ClawFs::len`] result, clamping if it somehow exceeds `usize` (a
    /// 32-bit device can only address `usize` bytes; transcript files are tiny).
    fn from_file_len(len: u64) -> ByteLen {
        ByteLen(usize::try_from(len).unwrap_or(usize::MAX))
    }
}

/// One committed turn, lent read-only to context providers.
///
/// Returned (as one element of the shared snapshot) by
/// [`turns`](Transcript::turns). Carries the turn's messages plus, for a
/// committed turn, its stable [`TurnId`], so an provider computing a
/// summarization boundary or a recent-tail cutoff can reason about *which* turns
/// it has covered.
///
/// The in-progress open turn appears as the trailing element with `id == None`
/// (volatile, unstamped); every earlier element is a committed turn with
/// `id == Some(_)`.
#[derive(Clone, Debug)]
pub struct Turn {
    /// The turn's stable chronological id, or `None` for the open turn.
    pub id: Option<TurnId>,
    /// The turn's messages, oldest-to-newest.
    pub messages: Vec<Value>,
}

/// On-disk record discriminator. Keeping this explicit preserves the existing
/// `"t":"group"` wire format without wrapping every record in a one-variant
/// enum.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordKind {
    Group,
}

/// One committed turn as stored on a line of the data `.jsonl`.
#[derive(Serialize, Deserialize)]
struct LogRecord {
    #[serde(rename = "t")]
    kind: RecordKind,
    id: TurnId,
    msgs: Vec<Value>,
}

/// One record's location inside the data `.jsonl`, as stored in the manifest.
#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    #[serde(rename = "t")]
    kind: RecordKind,
    off: ByteOffset,
    len: ByteLen,
    id: TurnId,
}

/// The index manifest: the layout of the data log, rewritten atomically.
#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// Data-log byte length this manifest describes; load tail-scans past it.
    covered_len: ByteLen,
    next_id: TurnId,
    live: Vec<IndexEntry>,
}

/// A committed turn plus its byte location in the data log (once flushed).
struct StoredGroup {
    id: TurnId,
    msgs: Vec<Value>,
    loc: Option<(ByteOffset, ByteLen)>,
}

/// One message currently being assembled from streaming fragments.
enum MessageDraft {
    User(String),
    Assistant {
        content: String,
        reasoning_content: String,
        tool_calls: Vec<Value>,
    },
    Tool {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
}

impl MessageDraft {
    fn into_message(self) -> Value {
        match self {
            Self::User(content) => json!({ "role": "user", "content": content }),
            Self::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => assistant_message(content, reasoning_content, tool_calls),
            Self::Tool {
                tool_call_id,
                content,
                is_error,
            } => json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            }),
        }
    }

    fn message(&self) -> Value {
        match self {
            Self::User(content) => json!({ "role": "user", "content": content }),
            Self::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => assistant_message(
                content.clone(),
                reasoning_content.clone(),
                tool_calls.clone(),
            ),
            Self::Tool {
                tool_call_id,
                content,
                is_error,
            } => json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            }),
        }
    }
}

fn assistant_message(content: String, reasoning_content: String, tool_calls: Vec<Value>) -> Value {
    let mut message = serde_json::Map::new();
    message.insert("role".to_owned(), json!("assistant"));
    if !content.is_empty() {
        message.insert("content".to_owned(), Value::String(content));
    }
    if !reasoning_content.is_empty() {
        message.insert(
            "reasoning_content".to_owned(),
            Value::String(reasoning_content),
        );
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Value::Object(message)
}

/// Volatile contents owned by the one live [`TurnHandle`].
#[derive(Default)]
struct OpenTurn {
    messages: Vec<Value>,
    draft: Option<MessageDraft>,
}

impl OpenTurn {
    fn finish_message(&mut self) -> bool {
        if let Some(draft) = self.draft.take() {
            self.messages.push(draft.into_message());
            true
        } else {
            false
        }
    }

    fn snapshot(&self) -> Vec<Value> {
        let mut messages = self.messages.clone();
        if let Some(draft) = &self.draft {
            messages.push(draft.message());
        }
        messages
    }

    /// Whether the open turn has produced no content yet (no finished messages
    /// and no draft in progress).
    fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.draft.is_none()
    }
}

/// A serialized data line awaiting its append, tagged with the turn it belongs
/// to so its `loc` can be written back once appended.
struct Pending {
    line: Vec<u8>,
    id: TurnId,
}

/// The lock-protected contents of the store.
#[derive(Default)]
struct StoreState {
    groups: Vec<StoredGroup>,
    /// The one in-progress turn, owned here while a [`TurnHandle`] is live.
    /// `Some` from [`TranscriptStore::open_turn`] until the handle drops.
    open_turn: Option<OpenTurn>,
    id_allocator: TurnIdAllocator,

    /// Records appended in memory but not yet written to the `.jsonl`.
    pending: Vec<Pending>,
    /// Current byte length of the `.jsonl` (also the next append offset).
    data_len: ByteLen,
    /// Data length the on-disk manifest currently describes.
    manifest_covered_len: ByteLen,

    /// Cached snapshot returned by [`TranscriptStore::turns`] (committed turns
    /// plus the open turn). Rebuilt lazily and shared as an `Arc`; invalidated
    /// whenever content changes.
    turns_cache: Option<Arc<Vec<Turn>>>,
    /// Monotonic committed-turn version. A non-empty turn advances it exactly
    /// once when it commits.
    turn_version: u64,
}

impl StoreState {
    /// Any draft or structural change makes the cached snapshot stale.
    fn invalidate_turns_cache(&mut self) {
        self.turns_cache = None;
    }

    /// A non-empty turn was committed.
    fn mark_turn_boundary(&mut self) {
        self.invalidate_turns_cache();
        self.turn_version = self.turn_version.saturating_add(1);
    }
}

/// The agent's complete transcript over one concrete filesystem instance.
pub struct TranscriptStore<F: ClawFs> {
    filesystem: Arc<F>,
    transcript_id: u32,
    data_path: String,
    index_path: String,
    /// Shared only with the one live [`TurnHandle`]. Filesystem ownership stays
    /// on the store, so all I/O remains statically dispatched through `F`.
    state: Arc<Mutex<StoreState>>,
}

impl<F: ClawFs> Drop for TranscriptStore<F> {
    /// Best-effort retry for a previous persistence failure.
    fn drop(&mut self) {
        persist(
            self.filesystem.as_ref(),
            self.transcript_id,
            &self.data_path,
            &self.index_path,
            self.state.as_ref(),
            true,
        );
    }
}

/// The agent's complete transcript: an append-only, verbatim record
/// of every turn. See the module docs for the storage layout.
///
/// Build one with [`new`](Self::new), append turns through the [`TurnHandle`]
/// returned by [`open_turn`](Self::open_turn), and read the turn-structured
/// transcript with [`turns`](Self::turns). Dropping a non-empty turn persists it
/// immediately; dropping the store retries any pending failed write. Drive a
/// single store from one thread.
///
/// # Examples
///
/// ```
/// # use claw_interface::MemFs;
/// # use claw_memory::{AssistantFragment, TranscriptStore};
/// let filesystem = std::sync::Arc::new(MemFs::new());
/// let store = TranscriptStore::new(filesystem, 42, "/data/transcripts")
///     .expect("a fresh MemFs has no data log, so the transcript starts empty");
///
/// // Each nested handle finishes its message on drop.
/// let turn = store.open_turn().unwrap();
/// {
///     let mut user = turn.user().unwrap();
///     user.append("what's the weather?");
/// }
/// {
///     let mut assistant = turn.assistant().unwrap();
///     assistant.append(AssistantFragment::Content("Sunny."));
/// }
/// drop(turn); // commits and persists the complete turn
///
/// // One committed turn carrying its two messages.
/// let turns = store.turns();
/// assert_eq!(turns.len(), 1);
/// assert_eq!(turns[0].messages.len(), 2);
/// ```
/// Type-erased transcript boundary used by the agent runtime.
///
/// Concrete stores retain their filesystem type and use static dispatch. The
/// runtime erases only this high-level transcript behavior so a
/// `TranscriptStore<MemFs>` and a platform-backed store have one owner type.
pub trait Transcript {
    /// Open the unique writable turn.
    fn open_turn(&self) -> Result<TurnHandle, TurnError>;

    /// A read-only snapshot of committed turns plus the visible open turn.
    fn turns(&self) -> Arc<Vec<Turn>>;

    /// Monotonic committed-turn version.
    fn turn_version(&self) -> u64;
}

/// One structured contribution appended through an [`AssistantHandle`].
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantFragment<'a> {
    /// User-visible assistant output.
    Content(&'a str),
    /// Provider reasoning content retained in the transcript.
    Reasoning(&'a str),
    /// One provider-shaped tool call object.
    ToolCall(Value),
}

/// Failure opening or mutating a transcript turn.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnError {
    /// A live handle already owns this store's uncommitted turn.
    #[error("a transcript turn is already open")]
    AlreadyOpen,
    /// A nested message handle is still live.
    #[error("a transcript message is already open")]
    MessageAlreadyOpen,
    /// The turn has already been committed.
    #[error("the transcript turn is no longer active")]
    Inactive,
}

/// Failure building a [`TranscriptStore`] from its on-disk log.
///
/// A *missing* transcript is not an error (it starts empty); a mismatched or
/// unreadable *index* is recovered by rebuilding from the data log. This is
/// returned only when the data log itself exists but cannot be read, so a real
/// I/O failure is never silently mistaken for an empty transcript (which would
/// then be overwritten on the next turn).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptInitError {
    /// The transcript data log exists but could not be read.
    #[error("transcript data log {path} is unreadable: {source}")]
    Unreadable {
        /// The data-log path that failed to load.
        path: String,
        /// The underlying filesystem error.
        #[source]
        source: FsError,
    },
}

/// Failure deleting one persisted transcript file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptDeleteError {
    /// A transcript file could not be removed.
    #[error("failed to delete transcript file {path}: {source}")]
    Delete {
        /// The file that could not be removed.
        path: String,
        /// The underlying filesystem error.
        #[source]
        source: FsError,
    },
}

/// Failure enumerating persisted transcript ids.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranscriptListError {
    /// The transcript directory could not be listed.
    #[error("failed to list transcript directory {path}: {source}")]
    List {
        /// The directory that could not be listed.
        path: String,
        /// The underlying filesystem error.
        #[source]
        source: FsError,
    },
    /// A transcript filename used a known extension but not a numeric id.
    #[error("invalid persisted transcript filename: {0}")]
    InvalidFilename(String),
}

impl<F: ClawFs> TranscriptStore<F> {
    /// List every transcript id represented by a data or index file in `dir`.
    ///
    /// A missing directory is empty. When only one half of a transcript remains,
    /// its id is still returned so the owner can reconcile or delete it.
    pub fn list_persisted_ids(filesystem: &F, dir: &str) -> Result<Vec<u32>, TranscriptListError> {
        let entries = match filesystem.list_dir(dir) {
            Ok(entries) => entries,
            Err(FsError::NotFound) => return Ok(Vec::new()),
            Err(source) => {
                return Err(TranscriptListError::List {
                    path: dir.to_owned(),
                    source,
                });
            }
        };
        let mut ids = BTreeSet::new();
        for entry in entries {
            let stem = entry
                .strip_suffix(DATA_EXT)
                .or_else(|| entry.strip_suffix(INDEX_EXT));
            let Some(stem) = stem else {
                continue;
            };
            let id = stem
                .parse()
                .map_err(|_| TranscriptListError::InvalidFilename(entry))?;
            ids.insert(id);
        }
        Ok(ids.into_iter().collect())
    }

    /// Build the store for `transcript_id`, restoring its persisted contents if
    /// present (missing or unreadable files start empty).
    ///
    /// Different ids map to different files under `dir`, so each transcript is
    /// stored independently. A mismatched or unreadable index is rebuilt from the
    /// data log during construction.
    ///
    /// # Examples
    ///
    /// ```
    /// # use claw_interface::MemFs;
    /// # use claw_memory::TranscriptStore;
    /// let filesystem = std::sync::Arc::new(MemFs::new());
    /// let store = TranscriptStore::new(filesystem, 7, "/data/transcripts")
    ///     .expect("a fresh MemFs has no data log, so the transcript starts empty");
    /// assert!(store.turns().is_empty()); // missing files start empty
    /// ```
    ///
    /// # Errors
    ///
    /// [`TranscriptInitError::Unreadable`] when the transcript *data log*
    /// exists but cannot be read. A missing transcript starts empty, and a
    /// corrupt/mismatched *index* is transparently rebuilt from the data log.
    pub fn new(
        filesystem: Arc<F>,
        transcript_id: u32,
        dir: &str,
    ) -> Result<Self, TranscriptInitError> {
        let data_path = transcript_path(dir, transcript_id, DATA_EXT);
        let index_path = transcript_path(dir, transcript_id, INDEX_EXT);
        let (mut state, needs_rebuild) = load_state(filesystem.as_ref(), &data_path, &index_path)
            .map_err(|source| TranscriptInitError::Unreadable {
            path: data_path.clone(),
            source,
        })?;
        if needs_rebuild {
            write_live_set_to_files(
                filesystem.as_ref(),
                &data_path,
                &index_path,
                &mut state,
                transcript_id,
            );
        }
        Ok(Self {
            filesystem,
            transcript_id,
            data_path,
            index_path,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Delete the persisted files for `transcript_id`.
    ///
    /// The index is removed before the data log. Missing files are treated as
    /// already deleted. Callers must first drop every live store for this id;
    /// a live store may otherwise persist again and recreate the files.
    ///
    /// # Errors
    ///
    /// Returns [`TranscriptDeleteError`] when either existing file cannot be
    /// removed.
    pub fn delete(
        filesystem: &F,
        transcript_id: u32,
        dir: &str,
    ) -> Result<(), TranscriptDeleteError> {
        delete_transcript_file(filesystem, transcript_path(dir, transcript_id, INDEX_EXT))?;
        delete_transcript_file(filesystem, transcript_path(dir, transcript_id, DATA_EXT))
    }

    /// A monotonic counter bumped once for every non-empty committed turn.
    pub fn turn_version(&self) -> u64 {
        self.lock_state().turn_version
    }

    /// Open a turn. Build each message through the nested handle returned by
    /// [`TurnHandle::user`], [`TurnHandle::assistant`], or
    /// [`TurnHandle::tool`]. Dropping the turn commits and persists the group.
    ///
    /// Only one turn may be open at a time. A second overlapping call returns
    /// [`TurnError::AlreadyOpen`] rather than permitting messages to interleave.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::AlreadyOpen`] while another live handle owns the
    /// volatile turn.
    pub fn open_turn(&self) -> Result<TurnHandle, TurnError> {
        {
            let mut state = self.lock_state();
            if state.open_turn.is_some() {
                return Err(TurnError::AlreadyOpen);
            }
            state.open_turn = Some(OpenTurn::default());
        }
        let filesystem = Arc::clone(&self.filesystem);
        let transcript_id = self.transcript_id;
        let data_path = self.data_path.clone();
        let index_path = self.index_path.clone();
        let state = Arc::clone(&self.state);
        let persist_state = Arc::clone(&state);
        Ok(TurnHandle {
            state,
            on_drop: Some(Box::new(move || {
                persist(
                    filesystem.as_ref(),
                    transcript_id,
                    &data_path,
                    &index_path,
                    persist_state.as_ref(),
                    false,
                );
            })),
        })
    }

    /// A read-only snapshot of every turn, oldest-to-newest: each committed turn
    /// (`id == Some(_)`) followed by the in-progress open turn (`id == None`)
    /// when it has any content.
    ///
    /// This is the sole read surface. The full verbatim model-facing transcript
    /// is `turns().iter().flat_map(|t| &t.messages)`; turn-boundary logic filters
    /// on `id`. Cached and shared as an `Arc`: repeated calls between mutations
    /// return a cheap refcount bump rather than rebuilding/cloning the transcript.
    pub fn turns(&self) -> Arc<Vec<Turn>> {
        let mut state = self.lock_state();
        if let Some(cached) = &state.turns_cache {
            return Arc::clone(cached);
        }
        let mut turns: Vec<Turn> = state
            .groups
            .iter()
            .map(|group| Turn {
                id: Some(group.id),
                messages: group.msgs.clone(),
            })
            .collect();
        if let Some(open_turn) = &state.open_turn {
            if !open_turn.is_empty() {
                turns.push(Turn {
                    id: None,
                    messages: open_turn.snapshot(),
                });
            }
        }
        let snapshot = Arc::new(turns);
        state.turns_cache = Some(Arc::clone(&snapshot));
        snapshot
    }

    fn lock_state(&self) -> MutexGuard<'_, StoreState> {
        lock_state(&self.state)
    }
}

impl<F: ClawFs> Transcript for TranscriptStore<F> {
    fn open_turn(&self) -> Result<TurnHandle, TurnError> {
        TranscriptStore::open_turn(self)
    }

    fn turns(&self) -> Arc<Vec<Turn>> {
        TranscriptStore::turns(self)
    }

    fn turn_version(&self) -> u64 {
        TranscriptStore::turn_version(self)
    }
}

/// The RAII scope for one transcript turn.
///
/// Open role-specific child scopes with [`user`](Self::user),
/// [`assistant`](Self::assistant), and [`tool`](Self::tool). Each child finishes
/// its message when dropped. Dropping this handle commits the complete turn and
/// persists the committed turn.
///
/// # Examples
///
/// ```
/// # use claw_interface::MemFs;
/// # use claw_memory::{AssistantFragment, TranscriptStore};
/// # let filesystem = std::sync::Arc::new(MemFs::new());
/// # let store = TranscriptStore::new(filesystem, 1, "/data/transcripts").unwrap();
/// let turn = store.open_turn().unwrap();
/// {
///     let mut user = turn.user().unwrap();
///     user.append("call the weather tool");
/// }
/// {
///     let mut assistant = turn.assistant().unwrap();
///     assistant.append(AssistantFragment::ToolCall(
///         serde_json::json!({"id":"c1"}),
///     ));
/// }
/// {
///     let mut tool = turn.tool("c1", false).unwrap();
///     tool.append(r#"{"temp_c":21}"#);
/// }
/// // Reads see the open turn (id == None) before it commits.
/// let turns = store.turns();
/// assert_eq!(turns.last().map(|t| (t.id, t.messages.len())), Some((None, 3)));
/// drop(turn); // commits and persists
/// ```
#[must_use = "the turn is committed and persisted when this handle is dropped"]
pub struct TurnHandle {
    state: Arc<Mutex<StoreState>>,
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl TurnHandle {
    /// Open a user-message scope.
    pub fn user(&self) -> Result<UserHandle<'_>, TurnError> {
        start_message(self.state.as_ref(), MessageDraft::User(String::new()))?;
        Ok(UserHandle {
            state: self.state.as_ref(),
        })
    }

    /// Open an assistant-message scope.
    pub fn assistant(&self) -> Result<AssistantHandle<'_>, TurnError> {
        start_message(
            self.state.as_ref(),
            MessageDraft::Assistant {
                content: String::new(),
                reasoning_content: String::new(),
                tool_calls: Vec::new(),
            },
        )?;
        Ok(AssistantHandle {
            state: self.state.as_ref(),
        })
    }

    /// Open a tool-result scope.
    pub fn tool(&self, tool_call_id: &str, is_error: bool) -> Result<ToolHandle<'_>, TurnError> {
        start_message(
            self.state.as_ref(),
            MessageDraft::Tool {
                tool_call_id: tool_call_id.to_owned(),
                content: String::new(),
                is_error,
            },
        )?;
        Ok(ToolHandle {
            state: self.state.as_ref(),
        })
    }
}

impl Drop for TurnHandle {
    fn drop(&mut self) {
        let committed = {
            let mut state = lock_state(self.state.as_ref());
            let Some(mut turn) = state.open_turn.take() else {
                return;
            };
            turn.finish_message();
            if turn.messages.is_empty() {
                false
            } else {
                let messages = turn.messages;
                let id = state.id_allocator.next();
                enqueue(&mut state, id, messages.clone());
                state.groups.push(StoredGroup {
                    id,
                    msgs: messages,
                    loc: None,
                });
                state.mark_turn_boundary();
                true
            }
        };
        if committed {
            if let Some(persist) = self.on_drop.take() {
                persist();
            }
        }
    }
}

/// A user-message scope. Its only mutation is [`append`](Self::append); drop
/// seals the accumulated content into the parent turn.
#[must_use = "the user message is finished when this handle is dropped"]
pub struct UserHandle<'a> {
    state: &'a Mutex<StoreState>,
}

impl UserHandle<'_> {
    /// Append one content fragment.
    pub fn append(&mut self, fragment: &str) {
        let mut state = lock_state(self.state);
        let Some(OpenTurn {
            draft: Some(MessageDraft::User(content)),
            ..
        }) = state.open_turn.as_mut()
        else {
            debug_assert!(false, "user handle lost its draft");
            return;
        };
        content.push_str(fragment);
        state.invalidate_turns_cache();
    }
}

impl Drop for UserHandle<'_> {
    fn drop(&mut self) {
        finish_message(self.state);
    }
}

/// An assistant-message scope. Its only mutation is
/// [`append`](Self::append); drop renders the accumulated structured assistant
/// message into the parent turn.
#[must_use = "the assistant message is finished when this handle is dropped"]
pub struct AssistantHandle<'a> {
    state: &'a Mutex<StoreState>,
}

impl AssistantHandle<'_> {
    /// Append output, reasoning, or one tool call.
    pub fn append(&mut self, fragment: AssistantFragment<'_>) {
        let mut state = lock_state(self.state);
        let Some(OpenTurn {
            draft:
                Some(MessageDraft::Assistant {
                    content,
                    reasoning_content,
                    tool_calls,
                }),
            ..
        }) = state.open_turn.as_mut()
        else {
            debug_assert!(false, "assistant handle lost its draft");
            return;
        };
        match fragment {
            AssistantFragment::Content(fragment) => content.push_str(fragment),
            AssistantFragment::Reasoning(fragment) => reasoning_content.push_str(fragment),
            AssistantFragment::ToolCall(tool_call) => tool_calls.push(tool_call),
        }
        state.invalidate_turns_cache();
    }
}

impl Drop for AssistantHandle<'_> {
    fn drop(&mut self) {
        finish_message(self.state);
    }
}

/// A tool-result scope. Its only mutation is [`append`](Self::append); drop
/// seals the result into the parent turn.
#[must_use = "the tool result is finished when this handle is dropped"]
pub struct ToolHandle<'a> {
    state: &'a Mutex<StoreState>,
}

impl ToolHandle<'_> {
    /// Append one result-content fragment.
    pub fn append(&mut self, fragment: &str) {
        let mut state = lock_state(self.state);
        let Some(OpenTurn {
            draft: Some(MessageDraft::Tool { content, .. }),
            ..
        }) = state.open_turn.as_mut()
        else {
            debug_assert!(false, "tool handle lost its draft");
            return;
        };
        content.push_str(fragment);
        state.invalidate_turns_cache();
    }
}

impl Drop for ToolHandle<'_> {
    fn drop(&mut self) {
        finish_message(self.state);
    }
}

fn start_message(state: &Mutex<StoreState>, draft: MessageDraft) -> Result<(), TurnError> {
    let mut state = lock_state(state);
    let Some(turn) = state.open_turn.as_mut() else {
        return Err(TurnError::Inactive);
    };
    if turn.draft.is_some() {
        return Err(TurnError::MessageAlreadyOpen);
    }
    turn.draft = Some(draft);
    state.invalidate_turns_cache();
    Ok(())
}

fn finish_message(state: &Mutex<StoreState>) {
    let mut state = lock_state(state);
    let Some(turn) = state.open_turn.as_mut() else {
        debug_assert!(false, "message handle outlived its turn");
        return;
    };
    if turn.finish_message() {
        state.invalidate_turns_cache();
    }
}

/// Serialize a group record to a data line and queue it for the next append.
fn enqueue(state: &mut StoreState, id: TurnId, msgs: Vec<Value>) {
    let record = LogRecord {
        kind: RecordKind::Group,
        id,
        msgs,
    };
    match serde_json::to_vec(&record) {
        Ok(mut line) => {
            line.push(b'\n');
            state.pending.push(Pending { line, id });
        }
        Err(err) => log::warn!("transcript record serialization failed: {err}"),
    }
}

/// Flush pending records (one `append`) and, when needed, rewrite the manifest.
fn persist<F: ClawFs>(
    filesystem: &F,
    transcript_id: u32,
    data_path: &str,
    index_path: &str,
    state: &Mutex<StoreState>,
    force_manifest: bool,
) {
    let mut state = lock_state(state);
    // Pending data always makes the manifest stale: after the append the data log
    // has records the index doesn't know about. Fold that into want_manifest so
    let has_pending = !state.pending.is_empty();
    let want_manifest = force_manifest || has_pending;
    if !want_manifest {
        return;
    }

    if !state.pending.is_empty() {
        let mut data_buf = Vec::new();
        let mut locs = Vec::with_capacity(state.pending.len());
        let mut off = state.data_len.as_offset();
        for pending in &state.pending {
            let len = ByteLen::of(&pending.line);
            data_buf.extend_from_slice(&pending.line);
            locs.push((pending.id, off, len));
            off = off.advance(len);
        }
        if let Err(err) = filesystem.append(data_path, &data_buf) {
            log::warn!("transcript {transcript_id}: data append failed: {err}");
            return;
        }
        state.data_len = off.as_len();
        for (id, off, len) in locs {
            set_loc(&mut state, id, off, len);
        }
        state.pending.clear();
    }
    if let Some(bytes) = build_manifest_bytes(&state, transcript_id) {
        match filesystem.write_atomic(index_path, &bytes) {
            Ok(()) => {
                state.manifest_covered_len = state.data_len;
            }
            Err(err) => {
                log::warn!("transcript {transcript_id}: index write failed: {err}");
            }
        }
    }
}

/// Rewrite `.jsonl` + `.json` from the in-memory turns in id order, updating
/// state locs to the new layout on success.
fn write_live_set_to_files<F: ClawFs>(
    filesystem: &F,
    data_path: &str,
    index_path: &str,
    state: &mut StoreState,
    transcript_id: u32,
) {
    let mut data_buf = Vec::new();
    let mut live = Vec::new();
    let mut locs: Vec<(TurnId, ByteOffset, ByteLen)> = Vec::new();
    let mut off = ByteOffset::default();

    for group in &state.groups {
        let record = LogRecord {
            kind: RecordKind::Group,
            id: group.id,
            msgs: group.msgs.clone(),
        };
        let Some(len) = append_line(&mut data_buf, &record, transcript_id) else {
            return;
        };
        live.push(IndexEntry {
            kind: RecordKind::Group,
            off,
            len,
            id: group.id,
        });
        locs.push((group.id, off, len));
        off = off.advance(len);
    }

    let manifest = Manifest {
        version: MANIFEST_VERSION,
        covered_len: off.as_len(),
        next_id: state.id_allocator.peek(),
        live,
    };
    let manifest_bytes = match serde_json::to_vec(&manifest) {
        Ok(bytes) => bytes,
        Err(err) => {
            log::warn!("transcript {transcript_id}: write_live manifest serialize failed: {err}");
            return;
        }
    };

    if let Err(err) = filesystem.write_atomic(data_path, &data_buf) {
        log::warn!("transcript {transcript_id}: write_live data write failed: {err}");
        return;
    }
    if let Err(err) = filesystem.write_atomic(index_path, &manifest_bytes) {
        log::warn!("transcript {transcript_id}: write_live index write failed: {err}");
        // Data file is the fresh truth; stale manifest is rebuilt on next load.
    }

    state.pending.clear();
    state.data_len = off.as_len();
    state.manifest_covered_len = off.as_len();
    for group in &mut state.groups {
        group.loc = None;
    }
    for (id, off, len) in locs {
        set_loc(state, id, off, len);
    }
}

/// Serialize `record` into `buf` with a trailing newline; returns the line length.
fn append_line(buf: &mut Vec<u8>, record: &LogRecord, transcript_id: u32) -> Option<ByteLen> {
    match serde_json::to_vec(record) {
        Ok(mut line) => {
            line.push(b'\n');
            let len = ByteLen::of(&line);
            buf.extend_from_slice(&line);
            Some(len)
        }
        Err(err) => {
            log::warn!("transcript {transcript_id}: serialize record failed: {err}");
            None
        }
    }
}

/// Record a flushed group's byte location back onto its in-memory entry.
fn set_loc(state: &mut StoreState, id: TurnId, off: ByteOffset, len: ByteLen) {
    if let Some(group) = state.groups.iter_mut().find(|g| g.id == id) {
        group.loc = Some((off, len));
    }
}

/// Build the manifest of the current turns (those already on disk).
fn build_manifest_bytes(state: &StoreState, transcript_id: u32) -> Option<Vec<u8>> {
    let mut live = Vec::new();
    for group in &state.groups {
        if let Some((off, len)) = group.loc {
            live.push(IndexEntry {
                kind: RecordKind::Group,
                off,
                len,
                id: group.id,
            });
        }
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        covered_len: state.data_len,
        next_id: state.id_allocator.peek(),
        live,
    };
    match serde_json::to_vec(&manifest) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            log::warn!("transcript {transcript_id}: manifest serialize failed: {err}");
            None
        }
    }
}

/// Return true if `record` is the type and id that `entry` claims.
fn verify_entry(entry: &IndexEntry, record: &LogRecord) -> bool {
    entry.kind == record.kind && entry.id == record.id
}

/// Load and rehydrate persisted state. Returns `(state, needs_rebuild)`.
/// `needs_rebuild` is true when a manifest existed but its entries did not match
/// the data log — the caller should rewrite both files from the recovered state.
///
/// # Errors
///
/// [`FsError`] when the data log exists but cannot be opened. A *missing* data
/// log ([`FsError::NotFound`]) yields an empty state, and index problems are
/// recovered from the data log rather than surfaced — so a genuine data-log I/O
/// fault is never silently treated as an empty transcript.
fn load_state<F: ClawFs>(
    filesystem: &F,
    data_path: &str,
    index_path: &str,
) -> Result<(StoreState, bool), FsError> {
    let mut state = StoreState::default();
    let mut covered_len = ByteLen::default();
    let mut manifest_next_id = TurnId::new(1);
    let mut mismatch = false;

    // One handle to the data log, reused for every indexed record read and the
    // tail scan below, instead of reopening the file per access. A missing log is
    // a fresh transcript; any other open failure is a real fault, surfaced so
    // the empty state is not mistaken for "no transcript".
    let mut data_file = match filesystem.open(data_path) {
        Ok(file) => Some(file),
        Err(FsError::NotFound) => None,
        Err(error) => return Err(error),
    };

    match filesystem.read(index_path) {
        Ok(bytes) => {
            if let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) {
                covered_len = manifest.covered_len;
                manifest_next_id = manifest.next_id;
                'entries: for entry in &manifest.live {
                    let (off, len) = (entry.off, entry.len);
                    let Some(file) = data_file.as_mut() else {
                        // The manifest references a data log that cannot be opened;
                        // rebuild from whatever the tail scan recovers.
                        mismatch = true;
                        break 'entries;
                    };
                    match file.read_exact_at(off.as_u64(), len.as_usize()) {
                        Ok(buf) => match parse_record(&buf) {
                            Some(record) if verify_entry(entry, &record) => {
                                apply_record(&mut state, record, Some((off, len)));
                            }
                            Some(_) => {
                                log::error!(
                                    "transcript load: manifest entry at offset {} does not \
                                     match data log record; rebuilding",
                                    off.as_u64()
                                );
                                mismatch = true;
                                break 'entries;
                            }
                            None => {
                                log::error!(
                                    "transcript load: manifest entry at offset {} could not \
                                     be parsed; rebuilding",
                                    off.as_u64()
                                );
                                mismatch = true;
                                break 'entries;
                            }
                        },
                        Err(err) => {
                            log::error!(
                                "transcript load: manifest entry at offset {} could not be \
                                 read: {err}; rebuilding",
                                off.as_u64()
                            );
                            mismatch = true;
                            break 'entries;
                        }
                    }
                }
            } else if data_file.is_some() {
                log::error!("transcript load: manifest could not be parsed; rebuilding");
                mismatch = true;
            }
        }
        Err(FsError::NotFound) => {
            if data_file.is_some() {
                mismatch = true;
            }
        }
        Err(err) => {
            log::error!("transcript load: manifest could not be read: {err}; rebuilding");
            if data_file.is_some() {
                mismatch = true;
            }
        }
    }

    if mismatch {
        state = StoreState::default();
        covered_len = ByteLen::default();
        manifest_next_id = TurnId::new(1);
    }

    let mut data_file_len = 0;
    if let Some(file) = data_file.as_ref() {
        if let Ok(len) = file.size() {
            data_file_len = len;
        }
    }
    let data_len = ByteLen::from_file_len(data_file_len);
    if data_len > covered_len {
        let extra = data_len.saturating_sub(covered_len);
        if let Some(file) = data_file.as_mut() {
            let tail = file.read_exact_at(covered_len.as_offset().as_u64(), extra.as_usize())?;
            scan_tail(&mut state, &tail, covered_len.as_offset());
        }
    }

    state.data_len = data_len;
    state.manifest_covered_len = covered_len;
    state.groups.sort_by_key(|g| g.id);
    state.id_allocator =
        TurnIdAllocator::starting_at(manifest_next_id.max(max_seen_id(&state).next()));
    Ok((state, mismatch))
}

/// Parse a newline-delimited tail buffer, applying each complete record.
fn scan_tail(state: &mut StoreState, tail: &[u8], base_off: ByteOffset) {
    let mut start = 0usize;
    let mut pos = base_off;
    for (i, byte) in tail.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line = &tail[start..i];
        let line_len = ByteLen(i.saturating_sub(start).saturating_add(1));
        if let Some(record) = parse_record(line) {
            apply_record(state, record, Some((pos, line_len)));
        }
        pos = pos.advance(line_len);
        start = i.saturating_add(1);
    }
}

/// Fold one decoded record into the live state.
fn apply_record(state: &mut StoreState, record: LogRecord, loc: Option<(ByteOffset, ByteLen)>) {
    state.groups.push(StoredGroup {
        id: record.id,
        msgs: record.msgs,
        loc,
    });
    state.turn_version = state.turn_version.saturating_add(1);
}

/// Highest committed turn id seen.
fn max_seen_id(state: &StoreState) -> TurnId {
    let mut max = TurnId::new(0);
    for group in &state.groups {
        if group.id > max {
            max = group.id;
        }
    }
    max
}

fn parse_record(bytes: &[u8]) -> Option<LogRecord> {
    if bytes.is_empty() {
        return None;
    }
    match serde_json::from_slice::<LogRecord>(bytes) {
        Ok(record) => Some(record),
        Err(err) => {
            log::warn!("transcript load: skipping unparseable record: {err}");
            None
        }
    }
}

/// Build a per-transcript path from the base dir, id, and extension.
fn transcript_path(dir: &str, transcript_id: u32, ext: &str) -> String {
    format!("{}/{transcript_id}{ext}", dir.trim_end_matches('/'))
}

fn delete_transcript_file<F: ClawFs>(
    filesystem: &F,
    path: String,
) -> Result<(), TranscriptDeleteError> {
    match filesystem.remove(&path) {
        Ok(()) | Err(FsError::NotFound) => Ok(()),
        Err(source) => Err(TranscriptDeleteError::Delete { path, source }),
    }
}

fn lock_state(state: &Mutex<StoreState>) -> MutexGuard<'_, StoreState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claw_interface::MemFs;

    #[test]
    fn invalid_index_is_rebuilt_from_data_log() {
        let filesystem = Arc::new(MemFs::new());
        let dir = "/transcript-index-rebuild";
        let index_path = transcript_path(dir, 1, INDEX_EXT);

        let store = TranscriptStore::<MemFs>::new(Arc::clone(&filesystem), 1, dir).unwrap();
        {
            let turn = store.open_turn().unwrap();
            {
                let mut user = turn.user().unwrap();
                user.append("persisted user");
            }
            {
                let mut assistant = turn.assistant().unwrap();
                assistant.append(AssistantFragment::Content("persisted reply"));
            }
        }
        // Turn drop persists immediately, so both files are already on disk.
        assert!(serde_json::from_slice::<Manifest>(&filesystem.read(&index_path).unwrap()).is_ok());

        filesystem
            .write_atomic(&index_path, b"{not valid json")
            .unwrap();
        let rebuilt = TranscriptStore::<MemFs>::new(Arc::clone(&filesystem), 1, dir).unwrap();
        let messages: String = rebuilt
            .turns()
            .iter()
            .flat_map(|turn| turn.messages.iter())
            .map(Value::to_string)
            .collect();
        assert!(messages.contains("persisted user"));
        assert!(messages.contains("persisted reply"));
        assert!(serde_json::from_slice::<Manifest>(&filesystem.read(&index_path).unwrap()).is_ok());
    }

    #[test]
    fn delete_removes_both_transcript_files_and_is_idempotent() {
        let filesystem = Arc::new(MemFs::new());
        let dir = "/transcript-delete";
        let data_path = transcript_path(dir, 7, DATA_EXT);
        let index_path = transcript_path(dir, 7, INDEX_EXT);

        let store = TranscriptStore::<MemFs>::new(Arc::clone(&filesystem), 7, dir).unwrap();
        {
            let turn = store.open_turn().unwrap();
            {
                let mut user = turn.user().unwrap();
                user.append("delete me");
            }
            {
                let mut assistant = turn.assistant().unwrap();
                assistant.append(AssistantFragment::Content("deleted"));
            }
        }
        drop(store);
        assert!(filesystem.exists(&data_path));
        assert!(filesystem.exists(&index_path));

        TranscriptStore::<MemFs>::delete(filesystem.as_ref(), 7, dir).unwrap();
        assert!(!filesystem.exists(&data_path));
        assert!(!filesystem.exists(&index_path));

        TranscriptStore::<MemFs>::delete(filesystem.as_ref(), 7, dir).unwrap();
    }

    #[test]
    fn list_persisted_ids_unions_data_and_index_files() {
        let filesystem = MemFs::new();
        let dir = "/transcript-list";
        filesystem
            .write_atomic(&transcript_path(dir, 3, DATA_EXT), b"")
            .unwrap();
        filesystem
            .write_atomic(&transcript_path(dir, 3, INDEX_EXT), b"")
            .unwrap();
        filesystem
            .write_atomic(&transcript_path(dir, 7, DATA_EXT), b"")
            .unwrap();
        filesystem
            .write_atomic(&format!("{dir}/unrelated"), b"")
            .unwrap();

        assert_eq!(
            TranscriptStore::<MemFs>::list_persisted_ids(&filesystem, dir).unwrap(),
            vec![3, 7]
        );
    }

    #[test]
    fn list_persisted_ids_rejects_invalid_transcript_filenames() {
        let filesystem = MemFs::new();
        let dir = "/transcript-list-invalid";
        filesystem
            .write_atomic(&format!("{dir}/not-an-id.jsonl"), b"")
            .unwrap();

        assert_eq!(
            TranscriptStore::<MemFs>::list_persisted_ids(&filesystem, dir),
            Err(TranscriptListError::InvalidFilename(
                "not-an-id.jsonl".to_owned()
            ))
        );
    }
}
