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
//! The store owns only committed turns, oldest-to-newest, each stamped with a
//! monotonic [`TurnId`]. An in-progress turn belongs exclusively to its
//! [`TurnHandle`] and is invisible to store readers until the handle commits.
//!
//! # It does not compact
//!
//! Compaction — summarizing an aged prefix so the transcript fits the model's
//! context window — is **not** this store's job. Compaction is a property of the
//! *LLM request*, not of the record: the summary is a derived artifact assembled
//! at request time by the agent layer's rolling-summary context adapter, which
//! *reads* this store ([`turns`](Transcript::turns)) and
//! keeps its own summary. This store never deletes a turn, never folds turns into
//! a summary, and has no token budget. It just stores and replays the verbatim
//! transcript. (Bounding on-disk growth, if ever needed, is a separate retention
//! concern — also not compaction.)
//!
//! # Turns are built through a guard
//!
//! Transcript content is buffered through the unique [`TurnHandle`] returned by
//! [`open_turn`](TranscriptStore::open_turn). User and assistant text may arrive
//! as fragments, but neither fragments nor finished messages become visible
//! through the store until the whole turn commits. Dropping an active handle
//! finishes its current draft and commits the turn. A hard cancellation calls
//! [`discard`](TurnHandle::discard) instead.
//!
//! # Threading
//!
//! All state mutation (appends, commits, persistence) happens on the
//! **foreground** thread that owns the store. A single store must be driven from
//! one thread; the `Arc`-backed state lets a reader (a context adapter) hold a
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
//! An **in-memory** store (built with [`TranscriptStore::in_memory`]) holds the
//! same transcript but never touches the filesystem: it starts empty and every
//! persist is a no-op. Subagents use it — their transcripts are context-management
//! scratch that is never enumerated or resumed, so it need not survive a restart.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use claw_interface::{ClawFile, ClawFs, FsError};

/// Minimum gap between persistence writes (flash-wear debounce).
const DEFAULT_PERSIST_DEBOUNCE: Duration = Duration::from_secs(5);
/// Per-transcript filenames: `{dir}/{id}{DATA_EXT|INDEX_EXT}`.
const DATA_EXT: &str = ".jsonl";
const INDEX_EXT: &str = ".json";
/// Manifest schema version, so a future layout change can be detected on load.
const MANIFEST_VERSION: u32 = 1;

/// A monotonic logical identifier for a turn.
///
/// Fixes chronological order and gives compaction (in the agent layer) a stable
/// handle for "the summary covers turns up to here". A distinct type from byte
/// offsets/lengths so the two can never be swapped: ordering is keyed on
/// `TurnId`, addressing on `ByteOffset`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TurnId(pub u64);

impl TurnId {
    /// Wrap a raw turn number. Named `new` so [`TurnIdAllocator`] can construct
    /// ids generically.
    const fn new(value: u64) -> Self {
        TurnId(value)
    }

    /// The id that immediately follows this one.
    fn next(self) -> TurnId {
        TurnId(self.0.saturating_add(1))
    }
}

claw_utils::define_id_allocator!(
    /// Hands out this transcript's [`TurnId`]s. Single-owner (a `StoreState`
    /// field mutated under the store lock), so the lock it needs is that outer
    /// one, not one of its own. Its position is persisted via
    /// [`peek`](TurnIdAllocator::peek) into the manifest and restored with
    /// [`starting_at`](TurnIdAllocator::starting_at).
    TurnIdAllocator(TurnId),
    TurnId(0)
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

/// One committed turn, lent read-only to context adapters.
///
/// Returned (as one element of the shared snapshot) by
/// [`turns`](Transcript::turns). Carries the turn's messages plus, for a
/// committed turn, its stable [`TurnId`], so an adapter computing a
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

/// One record as stored on a line of the data `.jsonl`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum LogRecord {
    /// A committed turn: its messages in order.
    Group { id: TurnId, msgs: Vec<Value> },
}

/// One record's location inside the data `.jsonl`, as stored in the manifest.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum IndexEntry {
    Group {
        off: ByteOffset,
        len: ByteLen,
        id: TurnId,
    },
}

/// The index manifest: the layout of the data log, rewritten atomically.
#[derive(Default, Serialize, Deserialize)]
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
    Assistant(String),
}

impl MessageDraft {
    fn into_message(self) -> Value {
        match self {
            Self::User(content) => json!({ "role": "user", "content": content }),
            Self::Assistant(content) => json!({ "role": "assistant", "content": content }),
        }
    }

    fn message(&self) -> Value {
        match self {
            Self::User(content) => json!({ "role": "user", "content": content }),
            Self::Assistant(content) => json!({ "role": "assistant", "content": content }),
        }
    }
}

/// Volatile contents owned by the one live [`TurnHandle`].
#[derive(Default)]
struct OpenTurn {
    messages: Vec<Value>,
    draft: Option<MessageDraft>,
}

impl OpenTurn {
    fn finish_draft(&mut self) {
        if let Some(draft) = self.draft.take() {
            self.messages.push(draft.into_message());
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
    /// `Some` from [`TranscriptStore::open_turn`] until the handle commits or
    /// discards; the handle is only a token that mutates this buffer.
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
    /// Monotonic content version, bumped on any content change (an open-turn
    /// append/finish or a commit/discard). A pull-based reader caches work keyed
    /// on this and recomputes only when it advances — see
    /// [`TranscriptStore::version`].
    version: u64,

    last_persist: Option<Instant>,

    /// The last data/index write failure, if any, cleared on the next successful
    /// persist. Persistence is best-effort (in-memory turns stay authoritative),
    /// but a caller can observe a failed write via
    /// [`TranscriptStore::last_persist_error`] instead of only a log line.
    last_persist_error: Option<FsError>,
}

impl StoreState {
    /// Content changed: drop the cached snapshot and bump the version so
    /// pull-based readers rebuild.
    fn mark_changed(&mut self) {
        self.turns_cache = None;
        self.version = self.version.saturating_add(1);
    }
}

/// Shared inner state — held behind an `Arc` so a context adapter can keep its
/// own clone of the store and read the same transcript the agent writes.
#[derive(Clone, Copy)]
struct PersistenceFns {
    append: fn(&str, &[u8]) -> Result<(), FsError>,
    write_atomic: fn(&str, &[u8]) -> Result<(), FsError>,
}

impl PersistenceFns {
    fn new<F: ClawFs>() -> Self {
        Self {
            append: F::append,
            write_atomic: F::write_atomic,
        }
    }
}

struct StoreInner {
    transcript_id: u32,
    data_path: String,
    index_path: String,
    /// When true this store is in-memory only: it loads nothing at construction
    /// and every persist is a no-op, so it never touches the filesystem. The
    /// paths are unused (left empty) in this mode.
    volatile: bool,
    persistence: PersistenceFns,
    state: Mutex<StoreState>,
}

impl Drop for StoreInner {
    /// Best-effort final checkpoint: when the last store clone (and any live
    /// [`TurnHandle`], which holds its own `Arc<StoreInner>`) is gone, flush any
    /// debounced-but-unwritten committed turns. A no-op for a volatile store or
    /// when nothing is pending.
    fn drop(&mut self) {
        persist(self, true);
    }
}

/// The agent's complete transcript: an append-only, verbatim record
/// of every turn. See the module docs for the storage layout.
///
/// Build one with [`new`](Self::new), append turns through the [`TurnHandle`]
/// returned by [`open_turn`](Self::open_turn), and read the turn-structured
/// transcript with [`turns`](Self::turns). Persistence is automatic (debounced,
/// with a best-effort flush when the store is dropped). Drive a single store
/// from one thread.
///
/// # Examples
///
/// ```
/// # use claw_interface::MemFs;
/// # use claw_memory::{AssistantFinish, TranscriptStore};
/// MemFs::new();
/// let store = TranscriptStore::<MemFs>::new(42, "/data/transcripts")
///     .expect("a fresh MemFs has no data log, so the transcript starts empty");
///
/// // One turn = one handle; the whole turn commits when the handle drops.
/// {
///     let mut turn = store.open_turn().unwrap();
///     turn.append_user("what's the weather?").unwrap();
///     turn.finish_user().unwrap();
///     turn.append_assistant("Sunny.").unwrap();
///     turn.finish_assistant(AssistantFinish::PlainText("Sunny.")).unwrap();
/// }
///
/// // One committed turn carrying its two messages.
/// let turns = store.turns();
/// assert_eq!(turns.len(), 1);
/// assert_eq!(turns[0].messages.len(), 2);
/// ```
pub struct TranscriptStore<F: ClawFs + 'static> {
    inner: Arc<StoreInner>,
    _fs: PhantomData<fn() -> F>,
}

/// Type-erased transcript boundary used by the agent runtime.
///
/// The concrete [`TranscriptStore<F>`] keeps its filesystem type parameter;
/// callers that do not care which filesystem backs it can own `dyn Transcript`.
/// Opening a turn returns one concrete, non-generic [`TurnHandle`].
pub trait Transcript {
    /// Open the transcript's unique writable turn.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::AlreadyOpen`] while another live handle owns it.
    fn open_turn(&self) -> Result<TurnHandle, TurnError>;

    /// A read-only snapshot of every turn, oldest-to-newest.
    ///
    /// Each committed turn is a [`Turn`] with `id == Some(_)`; the in-progress
    /// open turn, if it has any content, is the trailing element with
    /// `id == None`. Callers derive everything from this: the flat model-facing
    /// transcript is `turns().iter().flat_map(|t| &t.messages)`, the committed
    /// turns are those with an id, and the volatile tail is the `None` entry.
    ///
    /// Shared as an `Arc`; gate calls on [`version`](Self::version) to rebuild
    /// only when the transcript changed.
    fn turns(&self) -> Arc<Vec<Turn>>;

    /// Monotonic content version, bumped on any change (open-turn append/finish
    /// or commit/discard). Used by pull-based readers to cache work.
    fn version(&self) -> u64;
}

/// The authoritative assistant message used to finish a streamed draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantFinish<'a> {
    /// A complete backend-shaped assistant message object.
    RawJson(&'a str),
    /// A complete backend-shaped assistant message object.
    Value(Value),
    /// A complete plain-text assistant message.
    PlainText(&'a str),
}

/// Failure opening or mutating a transcript turn.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnError {
    /// A live handle already owns this store's uncommitted turn.
    #[error("a transcript turn is already open")]
    AlreadyOpen,
    /// The handle has already committed or discarded its turn.
    #[error("the transcript turn is no longer active")]
    Inactive,
    /// A user fragment was supplied while an assistant draft was open.
    #[error("cannot append user content while an assistant message is open")]
    UserWhileAssistantOpen,
    /// An assistant fragment was supplied while a user draft was open.
    #[error("cannot append assistant content while a user message is open")]
    AssistantWhileUserOpen,
    /// No user draft exists to finish.
    #[error("no user message is open")]
    NoUserMessage,
    /// A user draft is open where an assistant message must be finished.
    #[error("cannot finish an assistant message while a user message is open")]
    UserMessageOpen,
    /// A complete tool result cannot be inserted in the middle of a draft.
    #[error("cannot record a tool result while a message is open")]
    MessageOpenForToolResult,
    /// The backend-shaped assistant message was not valid JSON.
    #[error("invalid assistant message json: {0}")]
    InvalidAssistantJson(String),
    /// An earlier mutation failed, so committing could preserve an invalid turn.
    #[error("the transcript turn is poisoned")]
    Poisoned,
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

// Manual `Clone`: only the `Arc` is cloned, so this is cheap and does **not**
// require `F: Clone` (a `#[derive(Clone)]` would wrongly add that bound).
impl<F: ClawFs + 'static> Clone for TranscriptStore<F> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _fs: PhantomData,
        }
    }
}

impl<F: ClawFs + 'static> TranscriptStore<F> {
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
    /// MemFs::new();
    /// let store = TranscriptStore::<MemFs>::new(7, "/data/transcripts")
    ///     .expect("a fresh MemFs has no data log, so the transcript starts empty");
    /// assert!(store.turns().is_empty()); // missing files start empty
    /// ```
    ///
    /// # Errors
    ///
    /// [`TranscriptInitError::Unreadable`] when the transcript *data log*
    /// exists but cannot be read. A missing transcript starts empty, and a
    /// corrupt/mismatched *index* is transparently rebuilt from the data log.
    pub fn new(transcript_id: u32, dir: &str) -> Result<Self, TranscriptInitError> {
        let data_path = transcript_path(dir, transcript_id, DATA_EXT);
        let index_path = transcript_path(dir, transcript_id, INDEX_EXT);
        let (mut state, needs_rebuild) =
            load_state::<F>(&data_path, &index_path).map_err(|source| {
                TranscriptInitError::Unreadable {
                    path: data_path.clone(),
                    source,
                }
            })?;
        if needs_rebuild {
            write_live_set_to_files::<F>(&data_path, &index_path, &mut state, transcript_id);
        }
        Ok(Self {
            inner: Arc::new(StoreInner {
                transcript_id,
                data_path,
                index_path,
                volatile: false,
                persistence: PersistenceFns::new::<F>(),
                state: Mutex::new(state),
            }),
            _fs: PhantomData,
        })
    }

    /// Build an **in-memory** store for `transcript_id`: it starts empty, never
    /// reads or writes the filesystem, and every persist is a no-op.
    ///
    /// Used for transcripts that are pure context-management scratch and need not
    /// survive a restart (subagents), so no `dir` is required. The write/read API
    /// is otherwise identical to a persisting store.
    pub fn in_memory(transcript_id: u32) -> Self {
        Self {
            inner: Arc::new(StoreInner {
                transcript_id,
                data_path: String::new(),
                index_path: String::new(),
                volatile: true,
                persistence: PersistenceFns::new::<F>(),
                state: Mutex::new(StoreState::default()),
            }),
            _fs: PhantomData,
        }
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
    pub fn delete(transcript_id: u32, dir: &str) -> Result<(), TranscriptDeleteError> {
        delete_transcript_file::<F>(transcript_path(dir, transcript_id, INDEX_EXT))?;
        delete_transcript_file::<F>(transcript_path(dir, transcript_id, DATA_EXT))
    }

    /// A monotonic counter bumped whenever the transcript content changes (an
    /// open-turn append/finish or a commit/discard).
    ///
    /// Lets a pull-based reader cache output keyed on the transcript and rebuild
    /// only when this advances, without diffing [`turns`](Self::turns).
    pub fn version(&self) -> u64 {
        self.lock_state().version
    }

    /// Open a turn. Append streaming message fragments through the returned
    /// [`TurnHandle`]; the whole turn is committed as one group when the handle
    /// drops unless it is explicitly discarded.
    ///
    /// Takes `&self` (the open turn buffers in the `Arc`-backed state), but a
    /// single store must be driven from one thread.
    ///
    /// Only one turn may be open at a time. A second overlapping call returns
    /// [`TurnError::AlreadyOpen`] rather than permitting messages to interleave.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::AlreadyOpen`] while another live handle owns the
    /// volatile turn.
    pub fn open_turn(&self) -> Result<TurnHandle, TurnError> {
        let mut state = self.lock_state();
        if state.open_turn.is_some() {
            return Err(TurnError::AlreadyOpen);
        }
        state.open_turn = Some(OpenTurn::default());
        Ok(TurnHandle {
            inner: Arc::clone(&self.inner),
            active: true,
            poisoned: false,
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
        lock_state(&self.inner)
    }
}

impl<F: ClawFs + 'static> Transcript for TranscriptStore<F> {
    fn open_turn(&self) -> Result<TurnHandle, TurnError> {
        TranscriptStore::open_turn(self)
    }

    fn turns(&self) -> Arc<Vec<Turn>> {
        TranscriptStore::turns(self)
    }

    fn version(&self) -> u64 {
        TranscriptStore::version(self)
    }
}

/// The unique writer for one open transcript turn.
///
/// Obtained from [`TranscriptStore::open_turn`]. Holds an `Arc` into the store's
/// inner state so it carries no lifetime and can be stored across async
/// boundaries or inside structs like `BaseAgent`. Every method locks only for
/// the short in-memory mutation and never carries a lock across an await point.
/// Dropping an active, valid handle finishes its current draft and commits the
/// turn. [`discard`](Self::discard) is the explicit cancellation path.
///
/// # Examples
///
/// ```
/// # use claw_interface::MemFs;
/// # use claw_memory::{AssistantFinish, TranscriptStore};
/// # MemFs::new();
/// # let store = TranscriptStore::<MemFs>::new(1, "/data/transcripts").unwrap();
/// let mut turn = store.open_turn().unwrap();
/// turn.append_user("call the weather tool").unwrap();
/// turn.finish_user().unwrap();
/// turn.finish_assistant(AssistantFinish::RawJson(
///     r#"{"role":"assistant","tool_calls":[{"id":"c1"}]}"#,
/// )).unwrap();
/// turn.record_tool_result("c1", "{\"temp_c\":21}", false).unwrap();
/// // Reads see the open turn (id == None) before it commits.
/// let turns = store.turns();
/// assert_eq!(turns.last().map(|t| (t.id, t.messages.len())), Some((None, 3)));
/// turn.commit().unwrap(); // or just let it drop
/// ```
#[must_use = "dropping an active turn commits it; call discard for cancellation"]
pub struct TurnHandle {
    inner: Arc<StoreInner>,
    active: bool,
    poisoned: bool,
}

impl TurnHandle {
    /// Append one fragment to the current user message, opening its draft when
    /// necessary.
    pub fn append_user(&mut self, fragment: &str) -> Result<(), TurnError> {
        self.mutate(TurnMutation::AppendUser(fragment))
    }

    /// Finish the current user draft as one complete transcript message.
    pub fn finish_user(&mut self) -> Result<(), TurnError> {
        self.mutate(TurnMutation::FinishUser)
    }

    /// Append one fragment to the current assistant message, opening its draft
    /// when necessary.
    pub fn append_assistant(&mut self, fragment: &str) -> Result<(), TurnError> {
        self.mutate(TurnMutation::AppendAssistant(fragment))
    }

    /// Finish the assistant message with its authoritative final shape.
    ///
    /// A raw finish preserves provider-specific reasoning/tool-call fields. A
    /// plain-text finish is used for synthesized terminal messages. Either form
    /// may finish a message that emitted no visible deltas.
    pub fn finish_assistant(&mut self, finish: AssistantFinish<'_>) -> Result<(), TurnError> {
        let message = match finish {
            AssistantFinish::RawJson(raw) => serde_json::from_str(raw).map_err(|error| {
                self.poisoned = true;
                TurnError::InvalidAssistantJson(error.to_string())
            })?,
            AssistantFinish::Value(message) => message,
            AssistantFinish::PlainText(content) => {
                json!({ "role": "assistant", "content": content })
            }
        };
        self.mutate(TurnMutation::FinishAssistant(message))
    }

    /// Finish a streamed assistant draft without serializing its final shape.
    ///
    /// The text accumulated by [`append_assistant`](Self::append_assistant) is
    /// moved into the transcript. When the response contains no tool calls, a
    /// copy is returned for the task-completion event; tool rounds return
    /// `None` because their text is not a terminal response.
    pub fn finish_streamed_assistant(
        &mut self,
        reasoning_content: String,
        tool_calls: Vec<Value>,
    ) -> Result<Option<String>, TurnError> {
        self.mutate_with(move |turn| {
            let content = match turn.draft.take() {
                Some(MessageDraft::Assistant(content)) => content,
                Some(draft @ MessageDraft::User(_)) => {
                    turn.draft = Some(draft);
                    return Err(TurnError::UserMessageOpen);
                }
                None => String::new(),
            };
            let response = tool_calls.is_empty().then(|| content.clone());
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
            turn.messages.push(Value::Object(message));
            Ok(response)
        })
    }

    /// Record one complete tool result in the current turn.
    pub fn record_tool_result(
        &mut self,
        tool_call_id: &str,
        content: &str,
        is_error: bool,
    ) -> Result<(), TurnError> {
        self.mutate(TurnMutation::RecordToolResult {
            tool_call_id,
            content,
            is_error,
        })
    }

    /// Finish any remaining draft and commit this turn as one persisted group.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError::Poisoned`] after any earlier invalid mutation; the
    /// turn is discarded in that case.
    pub fn commit(mut self) -> Result<(), TurnError> {
        if !self.active {
            return Err(TurnError::Inactive);
        }
        if self.poisoned {
            discard_open_turn(&self.inner);
            self.active = false;
            return Err(TurnError::Poisoned);
        }
        commit_open_turn(&self.inner);
        self.active = false;
        Ok(())
    }

    /// Discard this volatile turn without committing or persisting it.
    pub fn discard(mut self) {
        if self.active {
            discard_open_turn(&self.inner);
            self.active = false;
        }
    }

    fn mutate(&mut self, mutation: TurnMutation<'_>) -> Result<(), TurnError> {
        self.mutate_with(|turn| apply_turn_mutation(turn, mutation))
    }

    fn mutate_with<T>(
        &mut self,
        apply: impl FnOnce(&mut OpenTurn) -> Result<T, TurnError>,
    ) -> Result<T, TurnError> {
        if !self.active {
            return Err(TurnError::Inactive);
        }
        let result = {
            let mut state = lock_state(&self.inner);
            let Some(turn) = state.open_turn.as_mut() else {
                return Err(TurnError::Inactive);
            };
            let result = apply(turn);
            if result.is_ok() {
                state.mark_changed();
            }
            result
        };
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

impl Drop for TurnHandle {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if self.poisoned {
            discard_open_turn(&self.inner);
        } else {
            commit_open_turn(&self.inner);
        }
        self.active = false;
    }
}

enum TurnMutation<'a> {
    AppendUser(&'a str),
    FinishUser,
    AppendAssistant(&'a str),
    FinishAssistant(Value),
    RecordToolResult {
        tool_call_id: &'a str,
        content: &'a str,
        is_error: bool,
    },
}

fn apply_turn_mutation(turn: &mut OpenTurn, mutation: TurnMutation<'_>) -> Result<(), TurnError> {
    match mutation {
        TurnMutation::AppendUser(fragment) => match turn.draft.as_mut() {
            Some(MessageDraft::User(content)) => {
                content.push_str(fragment);
                Ok(())
            }
            Some(MessageDraft::Assistant(_)) => Err(TurnError::UserWhileAssistantOpen),
            None => {
                turn.draft = Some(MessageDraft::User(fragment.to_owned()));
                Ok(())
            }
        },
        TurnMutation::FinishUser => match turn.draft.take() {
            Some(MessageDraft::User(content)) => {
                turn.messages
                    .push(json!({ "role": "user", "content": content }));
                Ok(())
            }
            Some(draft @ MessageDraft::Assistant(_)) => {
                turn.draft = Some(draft);
                Err(TurnError::NoUserMessage)
            }
            None => Err(TurnError::NoUserMessage),
        },
        TurnMutation::AppendAssistant(fragment) => match turn.draft.as_mut() {
            Some(MessageDraft::Assistant(content)) => {
                content.push_str(fragment);
                Ok(())
            }
            Some(MessageDraft::User(_)) => Err(TurnError::AssistantWhileUserOpen),
            None => {
                turn.draft = Some(MessageDraft::Assistant(fragment.to_owned()));
                Ok(())
            }
        },
        TurnMutation::FinishAssistant(message) => match turn.draft.take() {
            Some(draft @ MessageDraft::User(_)) => {
                turn.draft = Some(draft);
                Err(TurnError::UserMessageOpen)
            }
            Some(MessageDraft::Assistant(_)) | None => {
                turn.messages.push(message);
                Ok(())
            }
        },
        TurnMutation::RecordToolResult {
            tool_call_id,
            content,
            is_error,
        } => {
            if turn.draft.is_some() {
                return Err(TurnError::MessageOpenForToolResult);
            }
            turn.messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            }));
            Ok(())
        }
    }
}

fn commit_open_turn(inner: &StoreInner) {
    let due = {
        let mut state = lock_state(inner);
        let Some(mut open_turn) = state.open_turn.take() else {
            return;
        };
        open_turn.finish_draft();
        if open_turn.messages.is_empty() {
            return;
        }
        let msgs = open_turn.messages;
        let id = state.id_allocator.next();
        // An in-memory store never flushes, so skip enqueuing a pending line that
        // would otherwise accumulate unbounded.
        if !inner.volatile {
            enqueue(&mut state, id, msgs.clone(), inner.transcript_id);
        }
        state.groups.push(StoredGroup {
            id,
            msgs,
            loc: None,
        });
        // A committed turn changes the turn-structured snapshot (a new turn
        // appears), so invalidate caches and bump the version.
        state.mark_changed();
        persist_due(&state)
    };
    if due {
        persist(inner, false);
    }
}

fn discard_open_turn(inner: &StoreInner) {
    let mut state = lock_state(inner);
    let Some(open_turn) = state.open_turn.take() else {
        return;
    };
    if !open_turn.is_empty() {
        state.mark_changed();
    }
}

/// Serialize a group record to a data line and queue it for the next append.
fn enqueue(state: &mut StoreState, id: TurnId, msgs: Vec<Value>, transcript_id: u32) {
    let record = LogRecord::Group { id, msgs };
    match serde_json::to_vec(&record) {
        Ok(mut line) => {
            line.push(b'\n');
            state.pending.push(Pending { line, id });
        }
        Err(err) => log::warn!("transcript {transcript_id}: serialize record failed: {err}"),
    }
}

/// Flush pending records (one `append`) and, when needed, rewrite the manifest.
fn persist(inner: &StoreInner, force_manifest: bool) {
    // An in-memory store keeps everything in `state`; it never writes files.
    if inner.volatile {
        return;
    }
    let mut state = lock_state(inner);
    // Pending data always makes the manifest stale: after the append the data log
    // has records the index doesn't know about. Fold that into want_manifest so
    // the two files stay in sync on every write.
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
        if let Err(err) = (inner.persistence.append)(&inner.data_path, &data_buf) {
            log::warn!(
                "transcript {}: data append failed: {err}",
                inner.transcript_id
            );
            state.last_persist_error = Some(err);
            return;
        }
        state.data_len = off.as_len();
        for (id, off, len) in locs {
            set_loc(&mut state, id, off, len);
        }
        state.pending.clear();
    }
    state.last_persist = Some(Instant::now());

    if let Some(bytes) = build_manifest_bytes(&state, inner.transcript_id) {
        match (inner.persistence.write_atomic)(&inner.index_path, &bytes) {
            Ok(()) => {
                state.manifest_covered_len = state.data_len;
                state.last_persist_error = None;
            }
            Err(err) => {
                log::warn!(
                    "transcript {}: index write failed: {err}",
                    inner.transcript_id
                );
                state.last_persist_error = Some(err);
            }
        }
    }
}

/// Rewrite `.jsonl` + `.json` from the in-memory turns in id order, updating
/// state locs to the new layout on success.
fn write_live_set_to_files<F: ClawFs>(
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
        let record = LogRecord::Group {
            id: group.id,
            msgs: group.msgs.clone(),
        };
        let Some(len) = append_line(&mut data_buf, &record, transcript_id) else {
            return;
        };
        live.push(IndexEntry::Group {
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

    if let Err(err) = F::write_atomic(data_path, &data_buf) {
        log::warn!("transcript {transcript_id}: write_live data write failed: {err}");
        state.last_persist_error = Some(err);
        return;
    }
    if let Err(err) = F::write_atomic(index_path, &manifest_bytes) {
        log::warn!("transcript {transcript_id}: write_live index write failed: {err}");
        state.last_persist_error = Some(err);
        // Data file is the fresh truth; stale manifest is rebuilt on next load.
    } else {
        state.last_persist_error = None;
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
            live.push(IndexEntry::Group {
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

fn persist_due(state: &StoreState) -> bool {
    if state.pending.is_empty() {
        return false;
    }
    match state.last_persist {
        None => true,
        Some(at) => at.elapsed() >= DEFAULT_PERSIST_DEBOUNCE,
    }
}

/// Return true if `record` is the type and id that `entry` claims.
fn verify_entry(entry: &IndexEntry, record: &LogRecord) -> bool {
    match (entry, record) {
        (IndexEntry::Group { id: eid, .. }, LogRecord::Group { id: rid, .. }) => eid == rid,
    }
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
fn load_state<F: ClawFs>(data_path: &str, index_path: &str) -> Result<(StoreState, bool), FsError> {
    let mut state = StoreState::default();
    let mut covered_len = ByteLen::default();
    let mut manifest_next_id = TurnId::default();
    let mut mismatch = false;

    // One handle to the data log, reused for every indexed record read and the
    // tail scan below, instead of reopening the file per access. A missing log is
    // a fresh transcript; any other open failure is a real fault, surfaced so
    // the empty state is not mistaken for "no transcript".
    let mut data_file = match F::open(data_path) {
        Ok(file) => Some(file),
        Err(FsError::NotFound) => None,
        Err(error) => return Err(error),
    };

    match F::read(index_path) {
        Ok(bytes) => {
            if let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) {
                covered_len = manifest.covered_len;
                manifest_next_id = manifest.next_id;
                'entries: for entry in &manifest.live {
                    let (off, len) = entry_loc(entry);
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
        manifest_next_id = TurnId::default();
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
    match record {
        LogRecord::Group { id, msgs } => {
            state.groups.push(StoredGroup { id, msgs, loc });
        }
    }
}

/// Highest committed turn id seen.
fn max_seen_id(state: &StoreState) -> TurnId {
    let mut max = TurnId::default();
    for group in &state.groups {
        if group.id > max {
            max = group.id;
        }
    }
    max
}

fn entry_loc(entry: &IndexEntry) -> (ByteOffset, ByteLen) {
    match *entry {
        IndexEntry::Group { off, len, .. } => (off, len),
    }
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

fn delete_transcript_file<F: ClawFs>(path: String) -> Result<(), TranscriptDeleteError> {
    match F::remove(&path) {
        Ok(()) | Err(FsError::NotFound) => Ok(()),
        Err(source) => Err(TranscriptDeleteError::Delete { path, source }),
    }
}

fn lock_state(inner: &StoreInner) -> MutexGuard<'_, StoreState> {
    inner
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claw_interface::MemFs;

    #[test]
    fn invalid_index_is_rebuilt_from_data_log() {
        MemFs::new();
        let dir = "/transcript-index-rebuild";
        let index_path = transcript_path(dir, 1, INDEX_EXT);

        let store = TranscriptStore::<MemFs>::new(1, dir).unwrap();
        {
            let mut turn = store.open_turn().unwrap();
            turn.append_user("persisted user").unwrap();
            turn.finish_user().unwrap();
            turn.finish_assistant(AssistantFinish::RawJson(
                r#"{"role":"assistant","content":"persisted reply"}"#,
            ))
            .unwrap();
        }
        // The first commit persists immediately (no debounce yet), so the data
        // log and manifest are already on disk here.
        assert!(serde_json::from_slice::<Manifest>(&MemFs::read(&index_path).unwrap()).is_ok());

        MemFs::write_atomic(&index_path, b"{not valid json").unwrap();
        let rebuilt = TranscriptStore::<MemFs>::new(1, dir).unwrap();
        let messages: String = rebuilt
            .turns()
            .iter()
            .flat_map(|turn| turn.messages.iter())
            .map(Value::to_string)
            .collect();
        assert!(messages.contains("persisted user"));
        assert!(messages.contains("persisted reply"));
        assert!(serde_json::from_slice::<Manifest>(&MemFs::read(&index_path).unwrap()).is_ok());
    }

    #[test]
    fn delete_removes_both_transcript_files_and_is_idempotent() {
        MemFs::new();
        let dir = "/transcript-delete";
        let data_path = transcript_path(dir, 7, DATA_EXT);
        let index_path = transcript_path(dir, 7, INDEX_EXT);

        let store = TranscriptStore::<MemFs>::new(7, dir).unwrap();
        {
            let mut turn = store.open_turn().unwrap();
            turn.append_user("delete me").unwrap();
            turn.finish_user().unwrap();
            turn.finish_assistant(AssistantFinish::PlainText("deleted"))
                .unwrap();
        }
        drop(store);
        assert!(MemFs::exists(&data_path));
        assert!(MemFs::exists(&index_path));

        TranscriptStore::<MemFs>::delete(7, dir).unwrap();
        assert!(!MemFs::exists(&data_path));
        assert!(!MemFs::exists(&index_path));

        TranscriptStore::<MemFs>::delete(7, dir).unwrap();
    }
}
