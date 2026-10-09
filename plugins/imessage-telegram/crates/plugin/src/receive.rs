//! Telegram receive loop: a `getUpdates` long poll over one receive slot.
//!
//! Each session opens one kept-alive connection on the leased slot and polls
//! `getUpdates` with `timeout=50` and `allowed_updates=["message"]` until the
//! connection ends. The next poll's `offset` acknowledges every handled
//! update; the cursor is kept in RAM and stored at most once per
//! [`CURSOR_WRITE_INTERVAL`].

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
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
use embassy_time::{Duration, Instant};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::{ReceiveLease, ReceiveSlots};
use telegram::updates::{
    delete_webhook_path, first_update_id, get_updates_path, parse_acknowledgement, parse_updates,
    ApiError, Update, UpdatesError, POLL_TIMEOUT_SECS, UPDATE_BATCH,
};

use crate::channel::TelegramChannel;
use crate::CHANNEL;

/// Plugin storage key of the receive cursor: the next `getUpdates` offset as
/// a decimal string.
pub(crate) const CURSOR_STORAGE_KEY: &str = "offset";

/// Shortest time between two cursor writes.
pub(crate) const CURSOR_WRITE_INTERVAL: Duration = Duration::from_secs(10);

/// Largest response body one poll buffers. A batch over it is fetched again
/// one update at a time; a single update over it is skipped.
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024;

/// Bytes for a response's status line and headers.
const HEADER_BYTES: usize = 1024;

/// Limit for resolving, connecting, and the TLS handshake, and for the
/// `deleteWebhook` request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Limit for one poll: the server's hold time plus a margin.
const POLL_DEADLINE: Duration = Duration::from_secs(POLL_TIMEOUT_SECS as u64 + 20);

/// Limits of the receive connection.
const LIMITS: PollLimits = PollLimits {
    connect_timeout: CONNECT_TIMEOUT,
    request_timeout: POLL_DEADLINE,
    max_body: MAX_BODY_BYTES,
    max_tail: 0,
    max_head: HEADER_BYTES,
};

/// Update ids remembered to drop a redelivered update.
const RECENT_UPDATES: usize = 16;

/// The System's receive slots as the channel's slot source.
pub(crate) struct TelegramSlots<C: 'static, D: 'static>(pub(crate) ReceiveSlots<C, D>);

impl<C: 'static, D: 'static> ReceiveSlotSource for TelegramSlots<C, D> {
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

/// Where the next poll starts, and the updates handled most recently.
pub(crate) struct Cursor {
    /// Next `getUpdates` offset; `None` until the first update.
    offset: Cell<Option<i64>>,
    recent: RefCell<[Option<i64>; RECENT_UPDATES]>,
    next_recent: Cell<usize>,
    /// Offset last written to storage.
    stored: Cell<Option<i64>>,
    written_at: Cell<Option<Instant>>,
}

impl Cursor {
    /// Restores the stored offset; an unreadable one is dropped.
    pub(crate) async fn load<Storage: PluginStorage>(
        storage: &Storage,
    ) -> Result<Self, StorageError> {
        let stored = storage
            .get_bytes(CURSOR_STORAGE_KEY)
            .await?
            .and_then(|bytes| {
                let offset = core::str::from_utf8(&bytes).ok()?.parse::<i64>().ok();
                if offset.is_none() {
                    log::warn!("dropped an unreadable Telegram receive cursor");
                }
                offset
            });
        Ok(Self {
            offset: Cell::new(stored),
            recent: RefCell::new([None; RECENT_UPDATES]),
            next_recent: Cell::new(0),
            stored: Cell::new(stored),
            written_at: Cell::new(None),
        })
    }

    pub(crate) fn offset(&self) -> Option<i64> {
        self.offset.get()
    }

    /// Whether `id` was handled recently: the server sent it again.
    fn is_duplicate(&self, id: i64) -> bool {
        self.recent.borrow().contains(&Some(id))
    }

    /// Marks `id` handled; the next poll acknowledges it.
    fn advance(&self, id: i64) {
        let slot = self.next_recent.get();
        if let Some(entry) = self.recent.borrow_mut().get_mut(slot) {
            *entry = Some(id);
        }
        self.next_recent.set(
            slot.saturating_add(1)
                .checked_rem(RECENT_UPDATES)
                .unwrap_or(0),
        );
        self.offset.set(id.checked_add(1));
    }

    /// Forgets the position; a new bot starts from its own oldest update.
    pub(crate) fn clear(&self) {
        self.offset.set(None);
        self.recent.replace([None; RECENT_UPDATES]);
    }

    /// Stores the offset when it moved and the last write is at least
    /// [`CURSOR_WRITE_INTERVAL`] old.
    async fn persist<Storage: PluginStorage>(&self, storage: &Storage) {
        let offset = self.offset.get();
        if offset == self.stored.get() {
            return;
        }
        let now = Instant::now();
        if self
            .written_at
            .get()
            .is_some_and(|at| now.saturating_duration_since(at) < CURSOR_WRITE_INTERVAL)
        {
            return;
        }
        self.written_at.set(Some(now));
        let written = match offset {
            Some(offset) => {
                storage
                    .put(CURSOR_STORAGE_KEY, offset.to_string().as_bytes())
                    .await
            }
            None => storage.delete(CURSOR_STORAGE_KEY).await,
        };
        match written {
            Ok(()) => self.stored.set(offset),
            Err(error) => log::warn!("failed to store the Telegram receive cursor: {error}"),
        }
    }

    /// Deletes the stored offset after [`Self::clear`].
    pub(crate) async fn forget<Storage: PluginStorage>(
        &self,
        storage: &Storage,
    ) -> Result<(), StorageError> {
        storage.delete(CURSOR_STORAGE_KEY).await?;
        self.stored.set(None);
        Ok(())
    }
}

/// A failed connection or request as the receive error.
fn transport_error(error: GatewayHttpError) -> ReceiveError {
    match error {
        GatewayHttpError::Timeout => ReceiveError::new("Telegram did not answer in time"),
        GatewayHttpError::TlsNotConfigured => ReceiveError::halt("TLS is unavailable"),
        GatewayHttpError::Reqwless(error) => {
            ReceiveError::new(format!("Telegram connection failed: {error:?}"))
        }
        error => ReceiveError::new(format!("Telegram connection failed: {error}")),
    }
}

fn malformed(status: u16) -> ReceiveError {
    ReceiveError::new(format!("unreadable Telegram response (HTTP {status})"))
}

fn api_error(error: ApiError) -> ReceiveError {
    if error.is_bad_token() {
        return ReceiveError::halt(error.description);
    }
    let retry_after = error.retry_after;
    let receive_error = ReceiveError::new(error.description);
    match retry_after {
        Some(seconds) => receive_error.retry_after(Duration::from_secs(u64::from(seconds))),
        None => receive_error,
    }
}

impl<Storage, C, D> ReceiveChannel<ReceiveLease<C, D>> for TelegramChannel<Storage, C, D>
where
    Storage: PluginStorage,
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

/// The request a [`TelegramPoller`] has in flight.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InFlight {
    Updates,
    DeleteWebhook,
}

/// One session's polls: what to ask for next and what to do with answers.
struct TelegramPoller<'a, Storage, C: 'static, D: 'static> {
    channel: &'a TelegramChannel<Storage, C, D>,
    cursor: &'a Cursor,
    token: &'a str,
    sender: &'a Rc<dyn MessageChannel>,
    session: ReceiveSession<'a>,
    /// Updates the next poll asks for.
    limit: u8,
    /// Whether a poll on this connection succeeded, so a failure after it
    /// only means the server closed the kept-alive connection.
    healthy: bool,
    /// Whether the next request deletes the webhook.
    delete_webhook: bool,
    in_flight: InFlight,
}

impl<Storage, C, D> Poller for TelegramPoller<'_, Storage, C, D>
where
    Storage: PluginStorage,
    C: TcpConnect + 'static,
    D: Dns + 'static,
{
    type Error = ReceiveError;

    async fn next(&mut self) -> Result<PollRequest, ReceiveError> {
        if core::mem::replace(&mut self.delete_webhook, false) {
            // Once per entry into `send_receive`.
            self.channel.mark_webhook_cleared();
            log::info!("deleting the Telegram webhook so getUpdates can run");
            self.in_flight = InFlight::DeleteWebhook;
            return Ok(PollRequest::from(delete_webhook_path(self.token)).within(CONNECT_TIMEOUT));
        }
        self.channel.gateway.ready().await;
        self.in_flight = InFlight::Updates;
        Ok(get_updates_path(self.token, self.cursor.offset(), self.limit).into())
    }

    fn headers<'s>(&'s self, _headers: &mut Vec<(&'s str, &'s str)>) {}

    async fn response(&mut self, reply: Polled) -> Result<(), ReceiveError> {
        if self.in_flight == InFlight::DeleteWebhook {
            return match parse_acknowledgement(reply.status, &reply.body) {
                Ok(()) => Ok(()),
                Err(UpdatesError::Api(error)) => Err(api_error(error)),
                Err(UpdatesError::Malformed { status }) => Err(malformed(status)),
            };
        }
        let channel = self.channel;
        if reply.truncated {
            if self.limit > 1 {
                log::debug!("refetching an oversized Telegram batch one update at a time");
                self.limit = 1;
                return Ok(());
            }
            let Some(id) = first_update_id(&reply.body) else {
                return Err(ReceiveError::new("Telegram response is too large"));
            };
            log::warn!("skipped Telegram update {id}: larger than {MAX_BODY_BYTES} bytes");
            self.cursor.advance(id);
            self.cursor.persist(&channel.storage).await;
            self.limit = UPDATE_BATCH;
            return Ok(());
        }
        let updates = parse_updates(reply.status, &reply.body);
        drop(reply);
        match updates {
            Ok(updates) => {
                self.session.receiving();
                self.healthy = true;
                self.limit = UPDATE_BATCH;
                for update in updates {
                    channel.handle(update, self.cursor, self.sender).await;
                }
                self.cursor.persist(&channel.storage).await;
                Ok(())
            }
            Err(UpdatesError::Api(error))
                if error.is_webhook_conflict() && !channel.webhook_cleared() =>
            {
                self.delete_webhook = true;
                Ok(())
            }
            Err(UpdatesError::Api(error)) => Err(api_error(error)),
            Err(UpdatesError::Malformed { status }) => Err(malformed(status)),
        }
    }
}

impl<Storage, C, D> TelegramChannel<Storage, C, D>
where
    Storage: PluginStorage,
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
            return Err(ReceiveError::halt("Telegram is not set up"));
        };
        let cursor = self
            .cursor
            .get_or_load(|| Cursor::load(&self.storage))
            .await
            .map_err(|error| {
                ReceiveError::new(format!(
                    "failed to read the Telegram receive cursor: {error}"
                ))
            })?;
        let mut poller = TelegramPoller {
            channel: self,
            cursor,
            token: settings.config.token.as_str(),
            sender: &settings.sender,
            session,
            limit: UPDATE_BATCH,
            healthy: false,
            delete_webhook: false,
            in_flight: InFlight::Updates,
        };
        let api_base = settings.config.api_base.as_str();
        match poll(&lease.client_factory(), api_base, LIMITS, &mut poller).await {
            PollEnd::Connect(error) => Err(transport_error(error)),
            // A kept-alive connection the server closed after a healthy
            // poll: reconnect at once.
            PollEnd::Request { error, .. }
                if poller.healthy
                    && poller.in_flight == InFlight::Updates
                    && !matches!(error, GatewayHttpError::Timeout) =>
            {
                log::debug!("Telegram receive connection ended: {error:?}");
                Ok(())
            }
            PollEnd::Request { error, .. } => Err(transport_error(error)),
            PollEnd::Poller(error) => Err(error),
        }
    }

    /// Handles one update at most once.
    async fn handle(&self, update: Update, cursor: &Cursor, sender: &Rc<dyn MessageChannel>) {
        let id = update.update_id;
        if cursor.is_duplicate(id) {
            log::debug!("dropped Telegram update {id}: already handled");
            return;
        }
        cursor.advance(id);
        let Some(message) = update.message else {
            log::debug!("ignored Telegram update {id}: not a message");
            return;
        };
        let (Some(text), Some(from)) = (message.text, message.from) else {
            log::debug!("ignored Telegram update {id}: not a text message");
            return;
        };
        // A receiving channel is configured, so its owner book is loaded.
        let Some(owners) = self.owners() else {
            return;
        };
        let conversation_id = message.chat.id.to_string();
        match owners
            .classify(&from.id.to_string(), Some(&from.label()), &text)
            .await
        {
            Classification::Owner => {
                let inbound = GatewayInboundMessage {
                    route: GatewayRoute::new(CHANNEL, conversation_id),
                    message_id: message.message_id.to_string(),
                    text,
                };
                if let Err(error) = self.gateway.publish(inbound).await {
                    log::warn!("failed to publish a Telegram message: {error}");
                }
            }
            Classification::Paired => {
                let reply = SendMessageRequest::text(
                    MessageTarget::new(CHANNEL, conversation_id),
                    String::from(PAIRED_REPLY),
                );
                if let Err(error) = sender.send_message(reply).await {
                    log::warn!("failed to confirm a Telegram pairing: {error}");
                }
            }
            Classification::Ignored => {
                log::debug!("ignored Telegram update {id}: the sender is not an owner");
            }
        }
    }
}
