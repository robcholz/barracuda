//! Inkbox receive loop: a 3 s poll of the iMessage list over one kept-alive
//! connection on a receive slot.
//!
//! Inkbox has no long poll or push connection a device can reach, so each
//! session opens one connection on the leased slot and asks for
//! `GET /api/v1/imessage/messages?limit=50&start_datetime=<newest seen>`
//! every [`POLL_INTERVAL`] until the connection ends. The list is newest
//! first and `start_datetime` is inclusive, so ids already handled are
//! remembered in a small ring and dropped. The first poll of a new identity
//! (no cursor yet) only records where to start: earlier messages are not
//! replayed to the agent.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_channel::{
    ChannelControl, Classification, ReceiveChannel, ReceiveError, ReceiveFuture, ReceiveSession,
    ReceiveSlotSource, SlotWait, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    poll, Error as GatewayHttpError, PollEnd, PollLimits, PollRequest, Polled, Poller,
};
use barracuda_imessage_gateway_plugin::{
    GatewayInboundMessage, GatewayRoute, MessageChannel, MessageTarget, SendMessageRequest,
};
use barracuda_plugin::manager::{PluginStorage, StorageError};
use embassy_time::{Duration, Instant, Timer};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::{ReceiveLease, ReceiveSlots};
use inkbox::inbound::{messages_path, parse_messages, InboundMessage, POLL_LIMIT};
use serde::{Deserialize, Serialize};

use crate::InkboxState;

/// Plugin storage key of the receive cursor:
/// `{"start":"<created_at>","recent":["<id>",…]}`.
pub(crate) const CURSOR_STORAGE_KEY: &str = "cursor";

/// Time between two polls of a healthy connection.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Shortest time between two cursor writes.
pub(crate) const CURSOR_WRITE_INTERVAL: Duration = Duration::from_secs(10);

/// Largest response body one poll buffers. A larger page is asked for again
/// with half the limit.
pub(crate) const MAX_BODY_BYTES: usize = 32 * 1024;

/// Message ids remembered to drop a message the inclusive start repeats.
const RECENT_MESSAGES: usize = 32;

/// `start_datetime` for an identity whose list was empty at the first poll:
/// every later message is new.
const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Bytes for a response's status line and headers.
const HEADER_BYTES: usize = 2048;

/// Limit for resolving, connecting, and the TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Limit for one poll's request and response.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// Wait after a 429 without a usable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// The System's receive slots as the channel's slot source.
pub(crate) struct InkboxSlots<C: 'static, D: 'static>(pub(crate) ReceiveSlots<C, D>);

impl<C: 'static, D: 'static> ReceiveSlotSource for InkboxSlots<C, D> {
    type Lease = ReceiveLease<C, D>;

    fn acquire(&self) -> Option<Self::Lease> {
        self.0.acquire()
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }

    fn in_use(&self) -> usize {
        self.0.in_use()
    }

    fn wait_available(&self) -> SlotWait<'_> {
        Box::pin(self.0.wait_available())
    }
}

#[derive(Default, Deserialize, Serialize)]
struct StoredCursor {
    start: Option<String>,
    #[serde(default)]
    recent: Vec<String>,
}

/// Where the next poll starts, and the messages handled most recently.
pub(crate) struct Cursor {
    /// `created_at` of the newest message seen; `None` before the first poll.
    start: RefCell<Option<String>>,
    /// Ids handled most recently, oldest first.
    recent: RefCell<VecDeque<String>>,
    /// Whether the cursor moved since it was last stored.
    dirty: Cell<bool>,
    written_at: Cell<Option<Instant>>,
}

impl Cursor {
    /// Restores the stored cursor; an unreadable one is dropped.
    pub(crate) async fn load<Storage: PluginStorage>(
        storage: &Storage,
    ) -> Result<Self, StorageError> {
        let stored = storage
            .get_bytes(CURSOR_STORAGE_KEY)
            .await?
            .map(|bytes| {
                serde_json::from_slice::<StoredCursor>(&bytes).unwrap_or_else(|_| {
                    log::warn!("dropped an unreadable Inkbox receive cursor");
                    StoredCursor::default()
                })
            })
            .unwrap_or_default();
        let mut recent: VecDeque<String> = stored.recent.into();
        while recent.len() > RECENT_MESSAGES {
            recent.pop_front();
        }
        Ok(Self {
            start: RefCell::new(stored.start),
            recent: RefCell::new(recent),
            dirty: Cell::new(false),
            written_at: Cell::new(None),
        })
    }

    pub(crate) fn start(&self) -> Option<String> {
        self.start.borrow().clone()
    }

    fn seen(&self, id: &str) -> bool {
        self.recent.borrow().iter().any(|seen| seen == id)
    }

    fn remember(&self, id: &str) {
        let mut recent = self.recent.borrow_mut();
        if recent.len() >= RECENT_MESSAGES {
            recent.pop_front();
        }
        recent.push_back(id.into());
        self.dirty.set(true);
    }

    fn advance(&self, created_at: &str) {
        if self.start.borrow().as_deref() != Some(created_at) {
            self.start.replace(Some(created_at.into()));
            self.dirty.set(true);
        }
    }

    /// Stores the cursor when it moved and the last write is at least
    /// [`CURSOR_WRITE_INTERVAL`] old; `force` skips the interval.
    async fn persist<Storage: PluginStorage>(&self, storage: &Storage, force: bool) {
        if !self.dirty.get() {
            return;
        }
        let now = Instant::now();
        if !force
            && self
                .written_at
                .get()
                .is_some_and(|at| now.saturating_duration_since(at) < CURSOR_WRITE_INTERVAL)
        {
            return;
        }
        self.written_at.set(Some(now));
        let stored = StoredCursor {
            start: self.start(),
            recent: self.recent.borrow().iter().cloned().collect(),
        };
        let Ok(bytes) = serde_json::to_vec(&stored) else {
            return;
        };
        match storage.put(CURSOR_STORAGE_KEY, bytes.as_slice()).await {
            Ok(()) => self.dirty.set(false),
            Err(error) => log::warn!("failed to store the Inkbox receive cursor: {error}"),
        }
    }

    /// Forgets the position and the ring; the next poll starts afresh.
    pub(crate) async fn forget<Storage: PluginStorage>(
        &self,
        storage: &Storage,
    ) -> Result<(), StorageError> {
        self.start.replace(None);
        self.recent.borrow_mut().clear();
        self.dirty.set(false);
        storage.delete(CURSOR_STORAGE_KEY).await
    }
}

/// Maps a failed poll's status to the receive error.
fn status_error(reply: &Polled) -> ReceiveError {
    match reply.status {
        401 | 403 => ReceiveError::halt("Inkbox 拒绝了 API Key / Inkbox rejected the API key"),
        429 => ReceiveError::new("Inkbox rate limit").retry_after(
            reply
                .retry_after
                .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs),
        ),
        status => ReceiveError::new(format!("Inkbox answered HTTP {status}")),
    }
}

impl<Storage, T, C, D> ReceiveChannel<ReceiveLease<C, D>> for InkboxState<Storage, T, C, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    C: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn receive<'a>(
        &'a self,
        lease: &'a mut ReceiveLease<C, D>,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        Box::pin(self.poll(lease, session))
    }
}

/// Poll limits on the device.
const LIMITS: PollLimits = PollLimits {
    connect_timeout: CONNECT_TIMEOUT,
    request_timeout: POLL_DEADLINE,
    max_body: MAX_BODY_BYTES,
    max_tail: 0,
    max_head: HEADER_BYTES,
};

/// One session's polls: what to ask for next and what to do with answers.
struct InkboxPoller<'a, Storage, T: 'static, C: 'static, D: 'static> {
    state: &'a InkboxState<Storage, T, C, D>,
    cursor: &'a Cursor,
    sender: Rc<dyn MessageChannel>,
    headers: &'a [(&'a str, &'a str)],
    session: ReceiveSession<'a>,
    /// Messages the next poll asks for.
    limit: usize,
    /// Whether the poll in flight only looks for where messages start.
    baseline: bool,
    /// Whether the next poll goes out without waiting the interval.
    immediate: bool,
}

impl<Storage, T, C, D> Poller for InkboxPoller<'_, Storage, T, C, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    C: TcpConnect + 'static,
    D: Dns + 'static,
{
    type Error = ReceiveError;

    async fn next(&mut self) -> Result<PollRequest, ReceiveError> {
        if !core::mem::replace(&mut self.immediate, false) {
            Timer::after(self.state.poll_interval).await;
        }
        self.state.gateway.ready().await;
        let start = self.cursor.start();
        // The first poll of an identity only finds its newest message.
        self.baseline = start.is_none();
        let limit = if self.baseline { 1 } else { self.limit };
        Ok(messages_path(start.as_deref(), limit).into())
    }

    fn headers<'s>(&'s self, headers: &mut Vec<(&'s str, &'s str)>) {
        headers.extend_from_slice(self.headers);
    }

    async fn response(&mut self, reply: Polled) -> Result<(), ReceiveError> {
        if !(200..300).contains(&reply.status) {
            return Err(status_error(&reply));
        }
        if reply.truncated {
            if self.limit > 1 {
                self.limit = (self.limit / 2).max(1);
                self.immediate = true;
                log::debug!(
                    "asking Inkbox for {} messages: the page was too large",
                    self.limit
                );
                return Ok(());
            }
            return Err(ReceiveError::new("Inkbox message is too large"));
        }
        let messages = parse_messages(&reply.body).map_err(|error| {
            ReceiveError::new(format!("unreadable Inkbox message list: {error}"))
        })?;
        drop(reply);
        self.session.receiving();
        let state = self.state;
        if self.baseline {
            start_at(self.cursor, &messages);
        } else {
            if messages.len() >= self.limit
                && messages
                    .last()
                    .is_some_and(|oldest| !self.cursor.seen(&oldest.id))
            {
                log::warn!(
                    "more than {} Inkbox messages since the last poll; older ones are skipped",
                    self.limit
                );
            }
            self.limit = POLL_LIMIT;
            for message in messages.into_iter().rev() {
                state.handle(message, self.cursor, &self.sender).await;
            }
        }
        self.cursor.persist(&state.storage, self.baseline).await;
        Ok(())
    }
}

/// Records where an identity's messages start, without handling any.
fn start_at(cursor: &Cursor, messages: &[InboundMessage]) {
    match messages.first() {
        Some(newest) => {
            cursor.remember(&newest.id);
            cursor.advance(&newest.created_at);
        }
        None => cursor.advance(EPOCH),
    }
    log::info!("Inkbox receive starts after the newest stored message");
}

impl<Storage, T, C, D> InkboxState<Storage, T, C, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    C: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// One receive session: polls on one connection until it ends or fails.
    async fn poll(
        &self,
        lease: &mut ReceiveLease<C, D>,
        session: ReceiveSession<'_>,
    ) -> Result<(), ReceiveError> {
        let Some(settings) = self.settings() else {
            return Err(ReceiveError::halt("Inkbox is not set up"));
        };
        let headers = [
            ("X-API-Key", settings.config.api_key.as_str()),
            ("Accept", "application/json"),
        ];
        let cursor = self
            .cursor
            .get_or_load(|| Cursor::load(&self.storage))
            .await
            .map_err(|error| {
                ReceiveError::new(format!("failed to read the Inkbox receive cursor: {error}"))
            })?;
        let mut poller = InkboxPoller {
            state: self,
            cursor,
            sender: Rc::clone(&settings.sender),
            headers: &headers,
            session,
            limit: POLL_LIMIT,
            baseline: false,
            immediate: true,
        };
        let origin = settings.config.api_base.trim_end_matches('/');
        match poll(&lease.client_factory(), origin, LIMITS, &mut poller).await {
            PollEnd::Connect(GatewayHttpError::TlsNotConfigured) => {
                Err(ReceiveError::halt("TLS is unavailable"))
            }
            PollEnd::Connect(error) => Err(ReceiveError::new(format!(
                "Inkbox connection failed: {error}"
            ))),
            // A kept-alive connection the server closed after answering:
            // reconnect at once.
            PollEnd::Request { error, answered } if answered > 0 => {
                log::debug!("Inkbox receive connection ended: {error}");
                Ok(())
            }
            PollEnd::Request { error, .. } => {
                Err(ReceiveError::new(format!("Inkbox did not answer: {error}")))
            }
            PollEnd::Poller(error) => Err(error),
        }
    }

    /// Handles one listed message at most once.
    async fn handle(
        &self,
        message: InboundMessage,
        cursor: &Cursor,
        sender: &Rc<dyn MessageChannel>,
    ) {
        if cursor.seen(&message.id) {
            return;
        }
        cursor.remember(&message.id);
        cursor.advance(&message.created_at);
        if !message.is_inbound() {
            return;
        }
        let InboundMessage {
            id,
            conversation_id,
            remote_number,
            content,
            ..
        } = message;
        let text = content.as_deref().map(str::trim).unwrap_or_default();
        let (Some(conversation_id), Some(remote_number)) = (conversation_id, remote_number) else {
            log::debug!("ignored an Inkbox message without its conversation or sender");
            return;
        };
        if text.is_empty() {
            log::debug!("ignored an Inkbox message without text");
            return;
        }
        // A receiving channel is configured, so its owner book is loaded.
        let Some(owners) = self.owners() else {
            return;
        };
        match owners.classify(&remote_number, None, text).await {
            Classification::Owner => {
                let inbound = GatewayInboundMessage {
                    route: GatewayRoute::new(inkbox::CHANNEL, conversation_id),
                    message_id: id,
                    text: text.into(),
                };
                if let Err(error) = self.gateway.publish(inbound).await {
                    log::warn!("failed to publish an Inkbox message: {error}");
                }
            }
            Classification::Paired => {
                let reply = SendMessageRequest::text(
                    MessageTarget::new(inkbox::CHANNEL, conversation_id),
                    String::from(PAIRED_REPLY),
                );
                if let Err(error) = sender.send_message(reply).await {
                    log::warn!("failed to confirm an Inkbox pairing: {error}");
                }
            }
            Classification::Ignored => {
                log::debug!("ignored an Inkbox message: the sender is not an owner");
            }
        }
    }
}
