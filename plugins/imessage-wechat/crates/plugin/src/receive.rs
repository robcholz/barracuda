//! The WeChat receive loop: the iLink `getupdates` long poll over one
//! receive-slot lease.
//!
//! One session holds one kept-alive connection and long-polls on it until
//! the server closes it, a poll misses its deadline (an empty success: the
//! connection is reopened at once), or a poll fails. Text messages from
//! allowed accounts are published to the Gateway; a pairing message adds its
//! sender and is answered once; everything else is dropped. A batch too large
//! to buffer is skipped: its cursor is read from the reply's end and the loss
//! is counted.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_channel::{
    Classification, ReceiveChannel, ReceiveError, ReceiveFuture, ReceiveSession, ReceiveSlotSource,
    SlotWait, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    poll, Error as GatewayHttpError, PollEnd, PollLimits, PollRequest, Polled, Poller,
};
use barracuda_imessage_gateway_plugin::{
    GatewayInboundMessage, GatewayRoute, MessageChannel, MessageTarget, SendMessageRequest,
};
use barracuda_plugin::api::Entropy;
use barracuda_plugin::manager::PluginStorage;
use embassy_time::{Duration, Instant};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::{ReceiveLease, ReceiveSlots};
use wechat::{
    authorization, parse_oversized_updates, parse_updates, updates_body, updates_headers,
    wechat_uin, ContextTokens, InboundText, Updates, UpdatesError, Wechat, WechatConfig,
    DEFAULT_LONG_POLL_MS, MAX_UPDATES_BYTES, MAX_UPDATES_TAIL_BYTES, UPDATES_HEADER_BYTES,
    UPDATES_PATH,
};

use crate::{ChannelConfiguration, CHANNEL};

/// Receive-state message after iLink reports the bot session expired (-14).
pub(crate) const SESSION_EXPIRED_MESSAGE: &str = "需要重新扫码 / Scan again to relink";

/// Plugin storage key of the last `get_updates_buf`.
pub(crate) const CURSOR_STORAGE_KEY: &str = "get_updates_buf";

/// Plugin storage key of the latest `context_token` per user.
pub(crate) const CONTEXT_TOKENS_STORAGE_KEY: &str = "context_tokens";

/// Plugin storage key of the bot id of the last confirmed QR login.
pub(crate) const BOT_ID_STORAGE_KEY: &str = "bot_id";

/// Deadline for opening the connection (DNS, TCP, and TLS).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Limits of the receive connection; each poll names its own deadline.
const LIMITS: PollLimits = PollLimits {
    connect_timeout: CONNECT_TIMEOUT,
    request_timeout: PollTiming::DEVICE.max_long_poll,
    max_body: MAX_UPDATES_BYTES,
    max_tail: MAX_UPDATES_TAIL_BYTES,
    max_head: UPDATES_HEADER_BYTES,
};

/// Time limits of the long poll.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PollTiming {
    /// Time a poll may run past the long-poll timeout iLink asked for before
    /// it counts as an empty reply.
    pub(crate) margin: Duration,
    /// Bounds on the long-poll timeout iLink may ask for.
    pub(crate) min_long_poll: Duration,
    pub(crate) max_long_poll: Duration,
    /// A cursor that advanced without new messages is written at most this
    /// often; one that came with messages is written at once.
    pub(crate) idle_cursor_write: Duration,
}

impl PollTiming {
    /// Limits used on the device.
    pub(crate) const DEVICE: Self = Self {
        margin: Duration::from_secs(5),
        min_long_poll: Duration::from_secs(5),
        max_long_poll: Duration::from_secs(120),
        idle_cursor_write: Duration::from_secs(300),
    };
}

/// Message ids remembered to drop redeliveries after a reconnect.
const RECENT_MESSAGE_IDS: usize = 32;

/// iLink HTTP status asking the client to slow down.
const TOO_MANY_REQUESTS: u16 = 429;

/// Wait after a 429, which carries no `Retry-After` the poll can read.
const RATE_LIMIT_DELAY: Duration = Duration::from_secs(30);

/// The System's receive slot pool as the scaffold's slot source.
pub(crate) struct WechatSlots<R: 'static, D: 'static>(ReceiveSlots<R, D>);

impl<R, D> WechatSlots<R, D> {
    pub(crate) const fn new(slots: ReceiveSlots<R, D>) -> Self {
        Self(slots)
    }
}

impl<R: 'static, D: 'static> ReceiveSlotSource for WechatSlots<R, D> {
    type Lease = ReceiveLease<R, D>;

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

/// Receive bookkeeping kept across sessions.
pub(crate) struct InboundState {
    /// The `get_updates_buf` to send next; empty before the first reply.
    cursor: RefCell<String>,
    /// Whether `cursor` differs from the stored one.
    cursor_dirty: Cell<bool>,
    /// When the cursor was last written.
    cursor_written: Cell<Option<Instant>>,
    /// Latest `context_token` per user, shared with the send path.
    context_tokens: Rc<ContextTokens>,
    /// Recently delivered message ids, newest last.
    recent: RefCell<VecDeque<String>>,
    /// Long-poll timeout iLink asked for.
    long_poll: Cell<Duration>,
    /// Bumped when the linked bot changes, so a session of the old bot
    /// writes nothing more.
    epoch: Cell<u32>,
    /// Non-text, bot, and group messages dropped since boot.
    skipped: Cell<u32>,
    /// Batches skipped since boot because they exceeded
    /// [`MAX_UPDATES_BYTES`]; their messages are lost.
    oversized: Cell<u32>,
    timing: PollTiming,
    /// Messages the Gateway accepted, for tests.
    #[cfg(test)]
    pub(crate) published: RefCell<alloc::vec::Vec<GatewayInboundMessage>>,
}

impl InboundState {
    /// Restores the cursor and context tokens. Unreadable values start
    /// empty: the cursor only saves a replay and tokens return with the next
    /// message.
    pub(crate) async fn load<Storage: PluginStorage>(
        storage: &Storage,
        timing: PollTiming,
    ) -> Self {
        let cursor = match storage.get_bytes(CURSOR_STORAGE_KEY).await {
            Ok(Some(bytes)) => String::from_utf8(bytes).unwrap_or_default(),
            Ok(None) => String::new(),
            Err(error) => {
                log::warn!("failed to read the WeChat receive cursor: {error}");
                String::new()
            }
        };
        let context_tokens = match storage.get_bytes(CONTEXT_TOKENS_STORAGE_KEY).await {
            Ok(Some(bytes)) => ContextTokens::decode(&bytes).unwrap_or_else(|_error| {
                log::warn!("discarding unreadable WeChat context tokens");
                ContextTokens::new()
            }),
            Ok(None) => ContextTokens::new(),
            Err(error) => {
                log::warn!("failed to read the WeChat context tokens: {error}");
                ContextTokens::new()
            }
        };
        Self {
            cursor: RefCell::new(cursor),
            cursor_dirty: Cell::new(false),
            cursor_written: Cell::new(None),
            context_tokens: Rc::new(context_tokens),
            // Grows with the first messages: nothing is held while idle.
            recent: RefCell::new(VecDeque::new()),
            long_poll: Cell::new(Duration::from_millis(DEFAULT_LONG_POLL_MS)),
            epoch: Cell::new(0),
            skipped: Cell::new(0),
            oversized: Cell::new(0),
            timing,
            #[cfg(test)]
            published: RefCell::new(alloc::vec::Vec::new()),
        }
    }

    /// The context tokens shared with the provider's send path.
    pub(crate) fn context_tokens(&self) -> Rc<ContextTokens> {
        Rc::clone(&self.context_tokens)
    }

    /// Records the bot of a confirmed login. A different bot than the last
    /// one starts from an empty cursor without context tokens. A login that
    /// names no bot keeps everything, as nothing shows it changed.
    pub(crate) async fn relink<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        bot_id: Option<&str>,
    ) {
        let Some(bot_id) = bot_id else {
            return;
        };
        let stored = match storage.get_bytes(BOT_ID_STORAGE_KEY).await {
            Ok(stored) => stored,
            Err(error) => {
                log::warn!("failed to read the linked WeChat bot: {error}");
                None
            }
        };
        if stored.as_deref() == Some(bot_id.as_bytes()) {
            return;
        }
        log::info!("linked a different WeChat bot; clearing its receive state");
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.cursor.borrow_mut().clear();
        self.cursor_dirty.set(false);
        self.cursor_written.set(None);
        self.context_tokens.clear();
        self.recent.borrow_mut().clear();
        self.long_poll
            .set(Duration::from_millis(DEFAULT_LONG_POLL_MS));
        for key in [CURSOR_STORAGE_KEY, CONTEXT_TOKENS_STORAGE_KEY] {
            if let Err(error) = storage.delete(key).await {
                log::warn!("failed to clear the WeChat key {key}: {error}");
            }
        }
        if let Err(error) = storage.put(BOT_ID_STORAGE_KEY, bot_id.as_bytes()).await {
            log::warn!("failed to store the linked WeChat bot: {error}");
        }
    }

    /// Whether `message_id` was not delivered recently; remembers it.
    fn first_seen(&self, message_id: &str) -> bool {
        let mut recent = self.recent.borrow_mut();
        if recent.iter().any(|seen| seen == message_id) {
            return false;
        }
        if recent.len() >= RECENT_MESSAGE_IDS {
            recent.pop_front();
        }
        recent.push_back(message_id.into());
        true
    }

    /// Adopts the reply's long-poll timeout, bounded.
    fn adopt_long_poll(&self, timeout_ms: Option<u64>) {
        if let Some(timeout_ms) = timeout_ms {
            self.long_poll.set(
                Duration::from_millis(timeout_ms)
                    .max(self.timing.min_long_poll)
                    .min(self.timing.max_long_poll),
            );
        }
    }

    /// Moves to the reply's cursor and stores it when the batch had
    /// messages, or when it has not been written for a while.
    async fn advance_cursor<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        cursor: Option<String>,
        had_messages: bool,
    ) {
        if let Some(cursor) = cursor {
            if *self.cursor.borrow() != cursor {
                self.cursor.replace(cursor);
                self.cursor_dirty.set(true);
            }
        }
        if !self.cursor_dirty.get() {
            return;
        }
        let idle_write_due = self
            .cursor_written
            .get()
            .is_none_or(|written| written.elapsed() >= self.timing.idle_cursor_write);
        if !had_messages && !idle_write_due {
            return;
        }
        let cursor = self.cursor.borrow().clone();
        match storage.put(CURSOR_STORAGE_KEY, cursor.as_bytes()).await {
            Ok(()) => {
                self.cursor_dirty.set(false);
                self.cursor_written.set(Some(Instant::now()));
            }
            Err(error) => log::warn!("failed to store the WeChat receive cursor: {error}"),
        }
    }

    async fn store_context_tokens<Storage: PluginStorage>(&self, storage: &Storage) {
        let stored = match self.context_tokens.encode() {
            Ok(bytes) => {
                storage
                    .put(CONTEXT_TOKENS_STORAGE_KEY, bytes.as_slice())
                    .await
            }
            Err(_error) => return,
        };
        if let Err(error) = stored {
            log::warn!("failed to store the WeChat context tokens: {error}");
        }
    }
}

/// The scaffold's view of the channel: runs receive sessions over a lease.
pub(crate) struct WechatReceiver<Storage, T: 'static, D: 'static, R: 'static>(
    Rc<ChannelConfiguration<Storage, T, D, R>>,
);

impl<Storage, T, D, R> WechatReceiver<Storage, T, D, R> {
    pub(crate) const fn new(configuration: Rc<ChannelConfiguration<Storage, T, D, R>>) -> Self {
        Self(configuration)
    }
}

impl<Storage, T, D, R> ReceiveChannel<ReceiveLease<R, D>> for WechatReceiver<Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    fn receive<'a>(
        &'a self,
        lease: &'a mut ReceiveLease<R, D>,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        Box::pin(self.0.receive_session(lease, session))
    }
}

/// One session's long polls: what to ask for next and what to do with
/// answers.
struct WechatPoller<'a, Storage, T: 'static, D: 'static, R: 'static> {
    channel: &'a ChannelConfiguration<Storage, T, D, R>,
    provider: &'a Rc<Wechat<'static, T, D>>,
    config: &'a WechatConfig,
    authorization: String,
    /// `X-WECHAT-UIN` of the poll in flight.
    uin: String,
    session: ReceiveSession<'a>,
    /// The linked bot the session started for.
    epoch: u32,
}

impl<Storage, T, D, R> Poller for WechatPoller<'_, Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    type Error = ReceiveError;

    async fn next(&mut self) -> Result<PollRequest, ReceiveError> {
        let channel = self.channel;
        channel.gateway.ready().await;
        let body = updates_body(&channel.inbound.cursor.borrow()).map_err(|error| {
            ReceiveError::new(UpdatesError::Invalid(error.to_string()).to_string())
        })?;
        self.uin = channel.request_uin(self.config);
        let deadline = channel
            .inbound
            .long_poll
            .get()
            .checked_add(channel.inbound.timing.margin)
            .unwrap_or(channel.inbound.timing.max_long_poll);
        Ok(PollRequest::post(UPDATES_PATH.into(), body).within(deadline))
    }

    fn headers<'s>(&'s self, headers: &mut Vec<(&'s str, &'s str)>) {
        updates_headers(self.config, &self.authorization, &self.uin, headers);
    }

    async fn response(&mut self, reply: Polled) -> Result<(), ReceiveError> {
        let channel = self.channel;
        let updates = if reply.truncated {
            parse_oversized_updates(reply.status, &reply.body, &reply.tail)
        } else {
            parse_updates(reply.status, &reply.body)
        };
        let oversized = reply.truncated;
        drop(reply);
        let updates = match updates {
            Ok(updates) => updates,
            Err(UpdatesError::SessionExpired) => {
                log::warn!("the WeChat bot session expired; a new QR login is needed");
                return Err(ReceiveError::halt(SESSION_EXPIRED_MESSAGE));
            }
            Err(UpdatesError::Status(TOO_MANY_REQUESTS)) => {
                return Err(
                    ReceiveError::new("iLink asked to slow down").retry_after(RATE_LIMIT_DELAY)
                );
            }
            Err(error) => return Err(ReceiveError::new(error.to_string())),
        };
        if oversized && self.epoch == channel.inbound.epoch.get() {
            let inbound = &channel.inbound;
            inbound
                .oversized
                .set(inbound.oversized.get().saturating_add(1));
            log::warn!(
                "skipped a WeChat update batch over {} KiB; its messages are lost",
                MAX_UPDATES_BYTES / 1024
            );
        }
        self.session.receiving();
        channel
            .deliver(self.provider, updates, self.epoch, oversized)
            .await;
        Ok(())
    }
}

impl<Storage, T, D, R> ChannelConfiguration<Storage, T, D, R>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    R: TcpConnect + 'static,
{
    /// One receive session: one kept-alive connection, long-polled until it
    /// ends.
    async fn receive_session(
        &self,
        lease: &ReceiveLease<R, D>,
        session: ReceiveSession<'_>,
    ) -> Result<(), ReceiveError> {
        let Some(provider) = self.provider() else {
            return Err(ReceiveError::halt("WeChat is not linked"));
        };
        let config = provider.config();
        let mut poller = WechatPoller {
            channel: self,
            provider: &provider,
            config,
            authorization: authorization(config),
            uin: String::new(),
            session,
            epoch: self.inbound.epoch.get(),
        };
        match poll(
            &lease.client_factory(),
            &config.api_base,
            LIMITS,
            &mut poller,
        )
        .await
        {
            PollEnd::Connect(GatewayHttpError::TlsNotConfigured) => {
                Err(ReceiveError::halt("TLS is unavailable"))
            }
            PollEnd::Connect(GatewayHttpError::Timeout) => {
                Err(ReceiveError::new("iLink connection timed out"))
            }
            PollEnd::Connect(GatewayHttpError::Reqwless(error)) => Err(ReceiveError::new(
                alloc::format!("iLink unreachable: {error:?}"),
            )),
            PollEnd::Connect(error) => Err(ReceiveError::new(alloc::format!(
                "iLink unreachable: {error}"
            ))),
            // An unanswered long poll is an empty reply; the connection is
            // mid-request, so reopen it at once.
            PollEnd::Request {
                error: GatewayHttpError::Timeout,
                ..
            } => {
                poller.session.receiving();
                Ok(())
            }
            // The server closed the kept-alive connection after a healthy
            // poll: reconnect at once.
            PollEnd::Request { answered, .. } if answered > 0 => Ok(()),
            PollEnd::Request {
                error: GatewayHttpError::Reqwless(error),
                ..
            } => Err(ReceiveError::new(
                UpdatesError::Transport(alloc::format!("{error:?}")).to_string(),
            )),
            PollEnd::Request { error, .. } => Err(ReceiveError::new(
                UpdatesError::Transport(error.to_string()).to_string(),
            )),
            PollEnd::Poller(error) => Err(error),
        }
    }

    /// The `X-WECHAT-UIN` of one request: a fresh random value, or the
    /// configured one when the Platform has no entropy.
    fn request_uin(&self, config: &WechatConfig) -> String {
        let mut bytes = [0_u8; 4];
        match self.entropy.fill(&mut bytes) {
            Ok(()) => wechat_uin(u32::from_le_bytes(bytes)),
            Err(_unavailable) => config.x_wechat_uin.clone(),
        }
    }

    /// Handles one reply: delivers new text messages, then stores the
    /// context tokens and the cursor. The cursor past a skipped batch
    /// (`oversized`) is stored at once, like one that came with messages.
    async fn deliver(
        &self,
        provider: &Rc<Wechat<'static, T, D>>,
        updates: Updates,
        epoch: u32,
        oversized: bool,
    ) {
        let Updates {
            messages,
            skipped,
            get_updates_buf,
            longpolling_timeout_ms,
        } = updates;
        if epoch != self.inbound.epoch.get() {
            return;
        }
        if skipped > 0 {
            let skipped = u32::try_from(skipped).unwrap_or(u32::MAX);
            self.inbound
                .skipped
                .set(self.inbound.skipped.get().saturating_add(skipped));
            log::debug!("skipped {skipped} WeChat messages without user text");
        }
        self.inbound.adopt_long_poll(longpolling_timeout_ms);
        let had_messages = oversized || !messages.is_empty();
        let mut tokens_changed = false;
        for message in messages {
            if !self.inbound.first_seen(&message.message_id) {
                continue;
            }
            tokens_changed |= self.deliver_one(provider, message).await;
        }
        if epoch != self.inbound.epoch.get() {
            return;
        }
        if tokens_changed {
            self.inbound.store_context_tokens(&self.storage).await;
        }
        self.inbound
            .advance_cursor(&self.storage, get_updates_buf, had_messages)
            .await;
    }

    /// Classifies one message; returns whether a context token changed.
    async fn deliver_one(
        &self,
        provider: &Rc<Wechat<'static, T, D>>,
        message: InboundText,
    ) -> bool {
        let InboundText {
            message_id,
            from_user_id,
            context_token,
            text,
        } = message;
        // A receiving channel is linked, so its owner book is loaded.
        let Some(owners) = self.owners.get() else {
            return false;
        };
        let classification = owners.classify(&from_user_id, None, &text).await;
        if classification == Classification::Ignored {
            return false;
        }
        // Only allowed accounts get a token slot, so strangers cannot evict
        // an owner's token.
        let changed = context_token
            .is_some_and(|token| self.inbound.context_tokens.remember(&from_user_id, &token));
        match classification {
            Classification::Owner => {
                let inbound = GatewayInboundMessage {
                    route: GatewayRoute::new(CHANNEL, from_user_id),
                    message_id,
                    text,
                };
                #[cfg(test)]
                let published = inbound.clone();
                match self.gateway.publish(inbound).await {
                    Ok(()) => {
                        #[cfg(test)]
                        self.inbound.published.borrow_mut().push(published);
                    }
                    Err(error) => log::warn!("failed to publish a WeChat message: {error}"),
                }
            }
            Classification::Paired => {
                let target = MessageTarget::new(CHANNEL, from_user_id);
                let reply = SendMessageRequest::text(target, PAIRED_REPLY);
                if let Err(error) = provider.send_message(reply).await {
                    log::warn!("failed to answer a WeChat pairing: {error}");
                }
            }
            Classification::Ignored => {}
        }
        changed
    }
}

#[cfg(test)]
mod tests;
