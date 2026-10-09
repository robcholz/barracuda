//! QQ receive loop: the WebSocket gateway over one receive slot.
//!
//! Each session fetches the gateway URL (cached; QQ allows two `GET
//! /gateway` calls a minute), opens a WebSocket over the leased slot, waits
//! for Hello, then sends Resume when a session is stored and Identify
//! (intents `1<<25`) otherwise. While connected it heartbeats every
//! `heartbeat_interval` with the last sequence number, raced against the
//! cancel-safe read so a heartbeat is never late because a read is pending.
//! `C2C_MESSAGE_CREATE` and `GROUP_AT_MESSAGE_CREATE` become Gateway
//! messages for owners.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_channel::{
    ChannelControl, Classification, ReceiveChannel, ReceiveError, ReceiveFuture, ReceiveSession,
    ReceiveSlotSource, SlotWait, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, GatewayInboundMessage, GatewayRoute, MessageChannel, MessageTarget,
    SendMessageRequest,
};
use barracuda_plugin::api::Entropy;
use barracuda_plugin::manager::{PluginStorage, StorageError};
use embassy_futures::select::{select3, Either3};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::{ReceiveLease, ReceiveSlots, ReceiveSocket};
use qq::gateway::{
    self, close_action, event, op, CloseAction, Hello, InboundText, MessageEvent, Ready, Scene,
    Skipped,
};
use qq::TokenError;
use serde::{Deserialize, Serialize};
use ws_client::{Limits, Message, Request, Url, WsError, WsReader, WsWriter};

use crate::channel::{QQChannel, Settings};
use crate::CHANNEL;

/// Plugin storage key of the gateway session: `{"session_id","seq"}`.
pub(crate) const SESSION_STORAGE_KEY: &str = "session";

/// Shortest time between two writes of a moved sequence number.
pub(crate) const SESSION_WRITE_INTERVAL: Duration = Duration::from_secs(10);

/// Shortest time between two `GET /gateway` calls (QQ allows two a minute).
pub(crate) const GATEWAY_URL_INTERVAL: Duration = Duration::from_secs(30);

/// Wait after QQ refuses `GET /gateway`, which is usually its rate limit.
const GATEWAY_URL_RETRY: Duration = Duration::from_secs(60);

/// Limit for resolving, connecting, TLS, and the WebSocket handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Limit for the server's Hello after the handshake.
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// Shortest heartbeat interval accepted from the server.
const MIN_HEARTBEAT: Duration = Duration::from_secs(1);

/// Wait before retrying a failed proactive token refresh.
const TOKEN_RETRY: Duration = Duration::from_secs(30);

/// Message ids remembered to drop a redelivered message.
const RECENT_MESSAGES: usize = 16;

/// WebSocket buffers: QQ events are small JSON documents.
pub(crate) const WS_LIMITS: Limits = Limits {
    max_message: 64 * 1024,
    idle_buffer: 2 * 1024,
    max_handshake: 2 * 1024,
};

const USER_AGENT: &str = "barracuda";

/// The System's receive slots as the channel's slot source.
pub(crate) struct QQSlots<C: 'static, D: 'static>(pub(crate) ReceiveSlots<C, D>);

impl<C: 'static, D: 'static> ReceiveSlotSource for QQSlots<C, D> {
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

/// The stored form of [`GatewaySession`].
#[derive(Deserialize, Serialize)]
struct StoredSession {
    session_id: String,
    seq: u64,
}

/// The gateway session to resume: its id and the last sequence number seen.
///
/// The id is stored when READY names it; the sequence number at most once
/// per [`SESSION_WRITE_INTERVAL`].
pub(crate) struct GatewaySession {
    id: RefCell<Option<String>>,
    seq: Cell<Option<u64>>,
    stored_seq: Cell<Option<u64>>,
    written_at: Cell<Option<Instant>>,
}

impl GatewaySession {
    /// Restores the stored session; an unreadable one is dropped.
    pub(crate) async fn load<Storage: PluginStorage>(
        storage: &Storage,
    ) -> Result<Self, StorageError> {
        let stored = storage
            .get_bytes(SESSION_STORAGE_KEY)
            .await?
            .and_then(|bytes| {
                let session = serde_json::from_slice::<StoredSession>(&bytes).ok();
                if session.is_none() {
                    log::warn!("dropped an unreadable QQ gateway session");
                }
                session
            });
        let (id, seq) = match stored {
            Some(session) => (Some(session.session_id), Some(session.seq)),
            None => (None, None),
        };
        Ok(Self {
            id: RefCell::new(id),
            seq: Cell::new(seq),
            stored_seq: Cell::new(seq),
            written_at: Cell::new(None),
        })
    }

    /// The session id and sequence number to resume, if both are known.
    pub(crate) fn resumable(&self) -> Option<(String, u64)> {
        Some((self.id.borrow().clone()?, self.seq.get()?))
    }

    pub(crate) fn seq(&self) -> Option<u64> {
        self.seq.get()
    }

    fn advance(&self, seq: u64) {
        if self.seq.get().is_none_or(|current| seq > current) {
            self.seq.set(Some(seq));
        }
    }

    /// Records a new session from READY and stores it at once.
    async fn start<Storage: PluginStorage>(&self, storage: &Storage, id: String) {
        self.id.replace(Some(id));
        self.written_at.set(None);
        self.stored_seq.set(None);
        self.persist(storage).await;
    }

    /// Stores the session when its sequence number moved and the last write
    /// is at least [`SESSION_WRITE_INTERVAL`] old.
    async fn persist<Storage: PluginStorage>(&self, storage: &Storage) {
        let seq = self.seq.get();
        if seq == self.stored_seq.get() {
            return;
        }
        let now = Instant::now();
        if self
            .written_at
            .get()
            .is_some_and(|at| now.saturating_duration_since(at) < SESSION_WRITE_INTERVAL)
        {
            return;
        }
        let (Some(session_id), Some(seq)) = (self.id.borrow().clone(), seq) else {
            return;
        };
        self.written_at.set(Some(now));
        let Ok(bytes) = serde_json::to_vec(&StoredSession { session_id, seq }) else {
            return;
        };
        match storage.put(SESSION_STORAGE_KEY, bytes.as_slice()).await {
            Ok(()) => self.stored_seq.set(Some(seq)),
            Err(error) => log::warn!("failed to store the QQ gateway session: {error}"),
        }
    }

    /// Forgets the session; the next connection identifies anew.
    pub(crate) async fn clear<Storage: PluginStorage>(
        &self,
        storage: &Storage,
    ) -> Result<(), StorageError> {
        self.id.replace(None);
        self.seq.set(None);
        self.stored_seq.set(None);
        storage.delete(SESSION_STORAGE_KEY).await
    }
}

/// What a receiving QQ channel keeps across connections.
pub(crate) struct GatewayState {
    /// Session id and last sequence number, for Resume.
    pub(crate) session: GatewaySession,
    /// Message ids handled most recently, to drop redeliveries.
    recent: RecentMessages,
    /// Cached `GET /gateway` answer; QQ allows two calls a minute.
    pub(crate) gateway_url: RefCell<Option<String>>,
    /// When `GET /gateway` was last called.
    gateway_url_fetched: Cell<Option<Instant>>,
}

impl GatewayState {
    /// Restores the stored session.
    async fn load<Storage: PluginStorage>(storage: &Storage) -> Result<Self, StorageError> {
        Ok(Self {
            session: GatewaySession::load(storage).await?,
            recent: RecentMessages::default(),
            gateway_url: RefCell::new(None),
            gateway_url_fetched: Cell::new(None),
        })
    }

    /// Forgets the session, the recent messages, and the gateway URL: they
    /// belong to another bot.
    pub(crate) async fn clear<Storage: PluginStorage>(
        &self,
        storage: &Storage,
    ) -> Result<(), StorageError> {
        self.gateway_url.replace(None);
        self.recent.clear();
        self.session.clear(storage).await
    }
}

/// Message ids handled most recently, oldest first.
#[derive(Default)]
pub(crate) struct RecentMessages(RefCell<VecDeque<String>>);

impl RecentMessages {
    /// Records `id`; `false` when it was already handled.
    fn first_time(&self, id: &str) -> bool {
        let mut recent = self.0.borrow_mut();
        if recent.iter().any(|seen| seen == id) {
            return false;
        }
        if recent.len() >= RECENT_MESSAGES {
            recent.pop_front();
        }
        recent.push_back(id.into());
        true
    }

    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// One decoded gateway payload, owned so the reader can be used again.
enum Inbound {
    Hello(Duration),
    HeartbeatAck,
    /// The server asks for a heartbeat now.
    Heartbeat,
    Dispatch(Option<u64>, Dispatch),
    Reconnect,
    InvalidSession {
        resumable: bool,
    },
    Other(u8),
}

enum Dispatch {
    Ready(String),
    Resumed,
    Message(MessageEvent, Scene),
    Other,
}

fn decode(text: &str) -> Result<Inbound, ReceiveError> {
    let unreadable = |error: serde_json::Error| {
        ReceiveError::new(format!("unreadable QQ gateway payload: {error}"))
    };
    let envelope = gateway::parse_envelope(text).map_err(unreadable)?;
    Ok(match envelope.op {
        op::HELLO => {
            let hello: Hello = gateway::parse_data(text).map_err(unreadable)?;
            Inbound::Hello(Duration::from_millis(hello.heartbeat_interval).max(MIN_HEARTBEAT))
        }
        op::HEARTBEAT_ACK => Inbound::HeartbeatAck,
        op::HEARTBEAT => Inbound::Heartbeat,
        op::RECONNECT => Inbound::Reconnect,
        op::INVALID_SESSION => Inbound::InvalidSession {
            resumable: gateway::parse_data::<bool>(text).unwrap_or(false),
        },
        op::DISPATCH => {
            let dispatch = match envelope.t.as_deref() {
                Some(event::READY) => {
                    let ready: Ready = gateway::parse_data(text).map_err(unreadable)?;
                    Dispatch::Ready(ready.session_id)
                }
                Some(event::RESUMED) => Dispatch::Resumed,
                Some(event::C2C_MESSAGE_CREATE) => Dispatch::Message(
                    gateway::parse_data(text).map_err(unreadable)?,
                    Scene::Direct,
                ),
                Some(event::GROUP_AT_MESSAGE_CREATE) => {
                    Dispatch::Message(gateway::parse_data(text).map_err(unreadable)?, Scene::Group)
                }
                _ => Dispatch::Other,
            };
            Inbound::Dispatch(envelope.s, dispatch)
        }
        other => Inbound::Other(other),
    })
}

fn token_error(error: TokenError) -> ReceiveError {
    match error {
        TokenError::Rejected { code, message } => ReceiveError::halt(format!(
            "QQ 拒绝了 App Secret / QQ rejected the App Secret ({}): {}",
            code.as_deref().unwrap_or("-"),
            message.as_deref().unwrap_or("-")
        )),
        TokenError::Unavailable { message } => {
            ReceiveError::new(format!("QQ token unavailable: {message}"))
        }
    }
}

fn ws_error(error: WsError) -> ReceiveError {
    ReceiveError::new(format!("QQ gateway connection failed: {error}"))
}

/// What a connection uses of the channel: its settings and gateway state.
struct Link<'a, T: 'static, D: 'static> {
    settings: &'a Settings<T, D>,
    state: &'a GatewayState,
}

/// How a session ended without an error of its own.
enum Ended {
    /// Reconnect now (a healthy connection) or after backoff.
    Reconnect,
    Failed(ReceiveError),
}

impl<Storage, T, D> ReceiveChannel<ReceiveLease<ReceiveSocket, D>>
    for QQChannel<Storage, QQSlots<ReceiveSocket, D>, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn receive<'a>(
        &'a self,
        lease: &'a mut ReceiveLease<ReceiveSocket, D>,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        Box::pin(async move {
            let (settings, url, state) = self.prepare().await?;
            let mut stream = match with_timeout(CONNECT_TIMEOUT, lease.connect_stream(&url)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    return Err(ReceiveError::new(format!(
                        "QQ gateway connection failed: {error}"
                    )))
                }
                Err(_timeout) => {
                    return Err(ReceiveError::new("QQ gateway did not answer in time"))
                }
            };
            let (reader, writer) = stream
                .split()
                .await
                .map_err(|error| ReceiveError::new(format!("QQ gateway TLS failed: {error}")))?;
            self.run(&settings, state, &url, reader, writer, session)
                .await
        })
    }
}

impl<Storage, Slots, T, D> QQChannel<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// The settings, the gateway state (loaded by the first session), and
    /// the gateway URL for the next connection.
    pub(crate) async fn prepare(
        &self,
    ) -> Result<(alloc::rc::Rc<Settings<T, D>>, String, &GatewayState), ReceiveError> {
        let Some(settings) = self.settings() else {
            return Err(ReceiveError::halt("QQ is not set up"));
        };
        let state = self
            .receiving
            .get_or_load(|| GatewayState::load(&self.storage))
            .await
            .map_err(|error| {
                ReceiveError::new(format!("failed to read the QQ gateway session: {error}"))
            })?;
        // A rejected App Secret halts here; the token is cached otherwise.
        settings.sender.token().await.map_err(token_error)?;
        if let Some(url) = state.gateway_url.borrow().clone() {
            return Ok((settings, url, state));
        }
        let now = Instant::now();
        if let Some(fetched) = state.gateway_url_fetched.get() {
            let next = fetched.saturating_add(GATEWAY_URL_INTERVAL);
            if now < next {
                return Err(ReceiveError::new("waiting to ask QQ for its gateway again")
                    .retry_after(next.saturating_duration_since(now)));
            }
        }
        state.gateway_url_fetched.set(Some(now));
        let url = match settings.sender.gateway_url().await {
            Ok(url) => url,
            Err(ChannelError::Authentication) => {
                return Err(ReceiveError::halt(
                    "QQ 拒绝了凭据 / QQ rejected the credentials",
                ))
            }
            Err(ChannelError::Transport { message }) => {
                return Err(ReceiveError::new(format!(
                    "QQ gateway lookup failed: {message}"
                )))
            }
            Err(error) => {
                return Err(
                    ReceiveError::new(format!("QQ gateway lookup refused: {error}"))
                        .retry_after(GATEWAY_URL_RETRY),
                )
            }
        };
        if Url::parse(&url).is_err() {
            return Err(ReceiveError::new("QQ answered an unusable gateway URL")
                .retry_after(GATEWAY_URL_RETRY));
        }
        state.gateway_url.replace(Some(url.clone()));
        Ok((settings, url, state))
    }

    /// Runs the gateway protocol over an open byte stream until it ends.
    pub(crate) async fn run<R: Read, W: Write>(
        &self,
        settings: &Settings<T, D>,
        state: &GatewayState,
        url: &str,
        reader: R,
        writer: W,
        session: ReceiveSession<'_>,
    ) -> Result<(), ReceiveError> {
        let target = Url::parse(url).map_err(ws_error)?;
        let entropy = self.entropy.clone();
        let mask = move || {
            let mut mask = [0_u8; 4];
            let _unavailable = entropy.fill(&mut mask);
            mask
        };
        let request = Request {
            headers: &[("User-Agent", USER_AGENT)],
            ..Request::new(target, self.random_key())
        };
        let (mut reader, mut writer) = match with_timeout(
            CONNECT_TIMEOUT,
            ws_client::connect(reader, writer, &request, mask, WS_LIMITS),
        )
        .await
        {
            Ok(Ok(halves)) => halves,
            Ok(Err(error @ WsError::Handshake(_))) => {
                // A stale gateway URL; ask for a new one next time.
                state.gateway_url.replace(None);
                return Err(ws_error(error));
            }
            Ok(Err(error)) => return Err(ws_error(error)),
            Err(_timeout) => return Err(ReceiveError::new("QQ gateway did not upgrade in time")),
        };
        let interval = match with_timeout(HELLO_TIMEOUT, reader.next()).await {
            Ok(Ok(Message::Text(text))) => match decode(text)? {
                Inbound::Hello(interval) => interval,
                _ => return Err(ReceiveError::new("QQ gateway did not say Hello")),
            },
            Ok(Ok(_other)) => return Err(ReceiveError::new("QQ gateway did not say Hello")),
            Ok(Err(error)) => return Err(ws_error(error)),
            Err(_timeout) => return Err(ReceiveError::new("QQ gateway did not say Hello in time")),
        };
        let token = settings.sender.token().await.map_err(token_error)?;
        let login = match state.session.resumable() {
            Some((session_id, seq)) => {
                log::info!("resuming the QQ gateway session at {seq}");
                gateway::resume(&token, &session_id, seq)
            }
            None => gateway::identify(&token),
        }
        .map_err(|error| ReceiveError::new(format!("QQ login payload: {error}")))?;
        writer.send_text(&login).await.map_err(ws_error)?;
        drop(login);
        match self
            .connected(
                Link { settings, state },
                &token,
                &mut reader,
                &mut writer,
                interval,
                session,
            )
            .await
        {
            Ended::Reconnect => Ok(()),
            Ended::Failed(error) => Err(error),
        }
    }

    /// The connected loop: reads payloads, heartbeats, and refreshes the
    /// token until the connection ends.
    async fn connected<R: Read, W: Write, M: ws_client::MaskSource>(
        &self,
        link: Link<'_, T, D>,
        token: &str,
        reader: &mut WsReader<R>,
        writer: &mut WsWriter<W, M>,
        interval: Duration,
        session: ReceiveSession<'_>,
    ) -> Ended {
        let Link { settings, state } = link;
        let mut healthy = false;
        let mut heartbeat_at = Instant::now().saturating_add(interval);
        let mut awaiting_ack = false;
        let mut token_retry_at: Option<Instant> = None;
        loop {
            let refresh_at = settings
                .sender
                .token_refresh_at()
                .unwrap_or(Instant::MAX)
                .max(token_retry_at.unwrap_or(Instant::MIN));
            let reading = &mut *reader;
            let next = select3(
                async move {
                    self.gateway.ready().await;
                    reading.next().await
                },
                Timer::at(heartbeat_at),
                Timer::at(refresh_at),
            )
            .await;
            let inbound = match next {
                Either3::First(Ok(Message::Text(text))) => match decode(text) {
                    Ok(inbound) => inbound,
                    Err(error) => return Ended::Failed(error),
                },
                Either3::First(Ok(Message::Ping(payload))) => {
                    if let Err(error) = writer.pong(payload).await {
                        return self.dropped(healthy, error);
                    }
                    continue;
                }
                Either3::First(Ok(Message::Pong(_) | Message::Binary(_))) => continue,
                Either3::First(Ok(Message::Close(frame))) => {
                    let code = frame.map(|frame| frame.code);
                    let _closing = writer.close(ws_client::CLOSE_NORMAL, "").await;
                    return self.closed(settings, state, token, code, healthy).await;
                }
                Either3::First(Err(error)) => {
                    if let Some(code) = error.close_code() {
                        let _closing = writer.close(code, "").await;
                    }
                    return self.dropped(healthy, error);
                }
                Either3::Second(()) => {
                    if awaiting_ack {
                        return Ended::Failed(ReceiveError::new(
                            "QQ gateway stopped acknowledging heartbeats",
                        ));
                    }
                    if let Err(error) = self.heartbeat(writer, state).await {
                        return self.dropped(healthy, error);
                    }
                    awaiting_ack = true;
                    heartbeat_at = heartbeat_at.saturating_add(interval).max(Instant::now());
                    continue;
                }
                Either3::Third(()) => {
                    // Keep the shared token fresh while connected, for replies
                    // and the next Resume.
                    token_retry_at = match settings.sender.token().await {
                        Ok(_token) => None,
                        Err(error) => {
                            log::warn!("failed to refresh the QQ token: {error}");
                            Some(Instant::now().saturating_add(TOKEN_RETRY))
                        }
                    };
                    continue;
                }
            };
            match inbound {
                Inbound::HeartbeatAck => awaiting_ack = false,
                Inbound::Heartbeat => {
                    if let Err(error) = self.heartbeat(writer, state).await {
                        return self.dropped(healthy, error);
                    }
                }
                Inbound::Hello(_) => {}
                Inbound::Other(code) => log::debug!("ignored QQ gateway op {code}"),
                Inbound::Reconnect => {
                    log::info!("QQ gateway asked to reconnect");
                    return self.dropped(healthy, WsError::Eof);
                }
                Inbound::InvalidSession { resumable } => {
                    if !resumable {
                        if let Err(error) = state.session.clear(&self.storage).await {
                            log::warn!("failed to clear the QQ gateway session: {error}");
                        }
                    }
                    return Ended::Failed(ReceiveError::new("QQ gateway invalidated the session"));
                }
                Inbound::Dispatch(seq, dispatch) => {
                    if let Some(seq) = seq {
                        state.session.advance(seq);
                    }
                    match dispatch {
                        Dispatch::Ready(session_id) => {
                            log::info!("QQ gateway session ready");
                            state.session.start(&self.storage, session_id).await;
                            healthy = true;
                            session.receiving();
                        }
                        Dispatch::Resumed => {
                            log::info!("QQ gateway session resumed");
                            healthy = true;
                            session.receiving();
                        }
                        Dispatch::Message(message, scene) => {
                            self.handle(settings, state, message, scene).await;
                        }
                        Dispatch::Other => {}
                    }
                    state.session.persist(&self.storage).await;
                }
            }
        }
    }

    async fn heartbeat<W: Write, M: ws_client::MaskSource>(
        &self,
        writer: &mut WsWriter<W, M>,
        state: &GatewayState,
    ) -> Result<(), WsError> {
        let Ok(payload) = gateway::heartbeat(state.session.seq()) else {
            return Ok(());
        };
        writer.send_text(&payload).await
    }

    /// The connection failed or ended without a close code: resume.
    fn dropped(&self, healthy: bool, error: WsError) -> Ended {
        if healthy {
            log::info!("QQ gateway connection ended: {error}");
            Ended::Reconnect
        } else {
            Ended::Failed(ws_error(error))
        }
    }

    /// The server closed the connection with `code`.
    async fn closed(
        &self,
        settings: &Settings<T, D>,
        state: &GatewayState,
        token: &str,
        code: Option<u16>,
        healthy: bool,
    ) -> Ended {
        let Some(code) = code else {
            return self.dropped(healthy, WsError::Eof);
        };
        log::info!("QQ gateway closed the connection with {code}");
        match close_action(code) {
            CloseAction::Resume if healthy => Ended::Reconnect,
            CloseAction::Resume => Ended::Failed(ReceiveError::new(format!(
                "QQ gateway closed the connection ({code})"
            ))),
            CloseAction::Identify => {
                if let Err(error) = state.session.clear(&self.storage).await {
                    log::warn!("failed to clear the QQ gateway session: {error}");
                }
                if code == 4004 {
                    // Authentication failed: the next login needs a new token.
                    let _renewed = settings.sender.renew_token(token).await;
                }
                Ended::Failed(ReceiveError::new(format!(
                    "QQ gateway closed the connection ({code})"
                )))
            }
            CloseAction::Halt(message) => Ended::Failed(ReceiveError::halt(message)),
        }
    }

    /// Handles one message event at most once.
    async fn handle(
        &self,
        settings: &Settings<T, D>,
        state: &GatewayState,
        message: MessageEvent,
        scene: Scene,
    ) {
        if !state.recent.first_time(&message.id) {
            log::debug!("dropped a redelivered QQ message");
            return;
        }
        let InboundText {
            id,
            conversation_id,
            sender,
            label,
            text,
            scene,
        } = match message.into_text(scene) {
            Ok(text) => text,
            Err(Skipped::NotText) => {
                log::debug!("ignored a QQ message without text");
                return;
            }
            Err(Skipped::MissingSender) => {
                log::debug!("ignored a QQ message without its sender");
                return;
            }
        };
        settings.sender.note_inbound(&id, scene);
        // A receiving channel is configured, so its owner book is loaded.
        let Some(owners) = self.owners() else {
            return;
        };
        match owners.classify(&sender, label.as_deref(), &text).await {
            Classification::Owner => {
                let inbound = GatewayInboundMessage {
                    route: GatewayRoute::new(CHANNEL, conversation_id),
                    message_id: id,
                    text,
                };
                if let Err(error) = self.gateway.publish(inbound).await {
                    log::warn!("failed to publish a QQ message: {error}");
                }
            }
            Classification::Paired => {
                let mut reply = SendMessageRequest::text(
                    MessageTarget::new(CHANNEL, conversation_id),
                    String::from(PAIRED_REPLY),
                );
                // Bots may only answer, so the confirmation is a passive reply.
                reply.reply_to = Some(id);
                if let Err(error) = settings.sender.send_message(reply).await {
                    log::warn!("failed to confirm a QQ pairing: {error}");
                }
            }
            Classification::Ignored => {
                log::debug!("ignored a QQ message: the sender is not an owner");
            }
        }
    }
}
