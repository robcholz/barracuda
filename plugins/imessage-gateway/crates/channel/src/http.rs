//! Shared HTTP surface every channel exposes under its configuration path.
//!
//! A channel Plugin serves `/api/gateway/<channel>` and the `/status`, `/mode`
//! and `/owners` below it from one prefix registration, [`ChannelEndpoint`],
//! which hands the configuration path itself to the Plugin's own endpoint,
//! answers `/status` with [`status_response`], and hands the other two to
//! [`ModeEndpoint`] and [`OwnersEndpoint`]. One route per channel instead of
//! four keeps the webserver's route table small.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use barracuda_imessage_gateway_owners::{Owner, Owners};
use barracuda_plugin::manager::PluginStorage;
use barracuda_webserver_plugin::{HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mode::ChannelMode;
use crate::receive::{NoSlot, ReceiveControl, ReceiveSlotSource, ReceiveState};

/// Media type of every helper response.
pub const JSON_CONTENT_TYPE: &str = "application/json";

/// Why a channel could not apply a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModeError {
    /// Storing the mode failed; the previous mode still applies.
    #[error("failed to store the channel mode")]
    Storage,
    /// The Gateway rejected registering the channel.
    #[error("the Gateway rejected the channel")]
    Registration,
}

/// Future of [`ChannelControl::apply_mode`].
pub type ModeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ModeError>> + 'a>>;

/// What a channel Plugin implements to get the shared endpoints and status.
pub trait ChannelControl: 'static {
    /// The channel Plugin's storage.
    type Storage: PluginStorage;
    /// The channel's receive slot source.
    type Slots: ReceiveSlotSource;

    /// Whether the channel has a stored configuration.
    fn configured(&self) -> bool;

    /// The current mode.
    fn mode(&self) -> ChannelMode;

    /// Modes the channel offers; [`ChannelMode::ALL`] unless it overrides.
    fn modes(&self) -> &'static [ChannelMode] {
        ChannelMode::ALL
    }

    /// Stores `mode` (see [`crate::store_mode`]) and registers or
    /// unregisters the channel with the Gateway so that a registration exists
    /// exactly when it is configured and `mode.registers()`. After success
    /// [`Self::mode`] returns `mode`. The caller then updates
    /// [`Self::receive`].
    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_>;

    /// The channel's receive-loop control.
    fn receive(&self) -> &ReceiveControl<Self::Slots>;

    /// The channel's owner book, which a channel loads once it is configured
    /// (an [`crate::OnDemand`] keeps it); `None` before, so an unconfigured
    /// channel holds and reads none.
    fn owners(&self) -> Option<&Owners<Self::Storage>>;

    /// Channel-specific fields [`status_response`] adds after the shared
    /// ones, such as BlueBubbles' webhook counters. None by default.
    fn status_details(&self) -> Map<String, Value> {
        Map::new()
    }
}

/// Enables receiving exactly when `channel` is configured and in
/// `send_receive`. Call it after loading the mode in `register` and after
/// every configuration change; [`ModeEndpoint`] calls it after a mode change.
///
/// # Errors
///
/// Returns [`NoSlot`] when receiving should run but every slot is in use.
pub fn sync_receive<C: ChannelControl + ?Sized>(channel: &C) -> Result<(), NoSlot> {
    channel
        .receive()
        .set_enabled(channel.configured() && channel.mode().receives())
}

#[derive(Serialize)]
struct StatusBody<'a> {
    configured: bool,
    mode: ChannelMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    receive: Option<ReceiveBody<'a>>,
    owners: OwnerCount,
    #[serde(flatten)]
    details: Map<String, Value>,
}

#[derive(Serialize)]
struct ReceiveBody<'a> {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capacity: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slots: Option<SlotsBody>,
}

#[derive(Serialize)]
struct SlotsBody {
    in_use: usize,
    capacity: usize,
}

impl<'a> ReceiveBody<'a> {
    fn new(state: &'a ReceiveState, capacity: usize, in_use: usize) -> Self {
        Self {
            state: state.name(),
            message: state.message(),
            capacity: (*state == ReceiveState::NoSlot).then_some(capacity),
            // a channel that holds no outbound connection has no slot count to show
            slots: (capacity != usize::MAX).then_some(SlotsBody { in_use, capacity }),
        }
    }
}

#[derive(Serialize)]
struct OwnerCount {
    count: usize,
}

/// `GET /api/gateway/<channel>/status`:
/// `{"configured":bool,"mode":"…","receive":{"state":"…","message"?,"capacity"?,"slots"?:{"in_use":n,"capacity":n}},"owners":{"count":n}}`,
/// then the channel's [`ChannelControl::status_details`].
///
/// `receive` is present only in `send_receive`; `message` only with `error`;
/// `capacity` only with `no_slot`; `slots` whenever the channel draws from a
/// bounded slot pool.
pub fn status_response<C: ChannelControl + ?Sized>(channel: &C) -> HttpResponse {
    let mode = channel.mode();
    let state = channel.receive().state();
    to_json(
        200,
        &StatusBody {
            configured: channel.configured(),
            mode,
            receive: mode.receives().then(|| {
                ReceiveBody::new(
                    &state,
                    channel.receive().capacity(),
                    channel.receive().in_use(),
                )
            }),
            owners: OwnerCount {
                count: channel.owners().map_or(0, Owners::count),
            },
            details: channel.status_details(),
        },
    )
}

/// `POST /api/gateway/<channel>/mode` with `{"mode":"disabled"|"send"|"send_receive"}`.
///
/// Answers 204 when applied, or 409 `{"error":"no_slot","capacity":n}` when
/// `send_receive` finds every receive slot in use; the mode is saved anyway
/// and the channel takes a slot once one frees. 400 `invalid_request` for a
/// malformed body, 400 `unsupported_mode` for a mode the channel does not
/// offer, 500 `storage`, 422 `registration_failed`, 405 for other methods.
pub struct ModeEndpoint<C>(Rc<C>);

impl<C> ModeEndpoint<C> {
    /// Serves the mode endpoint of `channel`.
    #[must_use]
    pub const fn new(channel: Rc<C>) -> Self {
        Self(channel)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeBody {
    mode: ChannelMode,
}

#[derive(Serialize)]
struct NoSlotBody {
    error: &'static str,
    capacity: usize,
}

async fn set_mode<C: ChannelControl>(channel: &C, body: &[u8]) -> HttpResponse {
    let Ok(ModeBody { mode }) = serde_json::from_slice(body) else {
        return error(400, "invalid_request");
    };
    if !channel.modes().contains(&mode) {
        return error(400, "unsupported_mode");
    }
    match channel.apply_mode(mode).await {
        Ok(()) => {}
        Err(ModeError::Storage) => return error(500, "storage"),
        Err(ModeError::Registration) => return error(422, "registration_failed"),
    }
    match sync_receive(channel) {
        Ok(()) => HttpResponse::new(204, JSON_CONTENT_TYPE, Vec::new()),
        Err(NoSlot) => to_json(
            409,
            &NoSlotBody {
                error: "no_slot",
                capacity: channel.receive().capacity(),
            },
        ),
    }
}

async fn mode_request<C: ChannelControl>(channel: &C, request: &HttpRequest) -> HttpResponse {
    match request.method() {
        HttpMethod::Post => set_mode(channel, request.body()).await,
        _ => error(405, "method_not_allowed"),
    }
}

impl<C: ChannelControl> HttpEndpoint for ModeEndpoint<C> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move { mode_request(self.0.as_ref(), &request).await })
    }
}

/// `GET` and `POST` on `/api/gateway/<channel>/owners`.
///
/// `GET` answers
/// `{"owners":[{"id":"…","label":"…"|null}],"pairing":{"code":"012345","expires_in":600}|null,"ignored":n}`,
/// minting a pairing code when none is valid; `pairing` is `null` while the
/// list is full or the Platform has no entropy. `POST` takes
/// `{"remove":"<id>"}` or `{"rotate":true}` and answers 204; 400
/// `invalid_request` for any other body, 500 `storage` when a removal cannot
/// be stored, 503 `entropy_unavailable` when no code can be minted. Both
/// answer 409 `not_configured` while the channel is not configured.
pub struct OwnersEndpoint<C>(Rc<C>);

impl<C> OwnersEndpoint<C> {
    /// Serves the owners endpoint of `channel`.
    #[must_use]
    pub const fn new(channel: Rc<C>) -> Self {
        Self(channel)
    }
}

#[derive(Serialize)]
struct OwnersBody {
    owners: Vec<Owner>,
    pairing: Option<PairingBody>,
    ignored: u32,
}

#[derive(Serialize)]
struct PairingBody {
    code: String,
    expires_in: u64,
}

#[derive(Deserialize)]
enum OwnersCommand {
    #[serde(rename = "remove")]
    Remove(String),
    #[serde(rename = "rotate")]
    Rotate(bool),
}

fn list_owners<C: ChannelControl>(channel: &C) -> HttpResponse {
    let Some(owners) = channel.owners() else {
        return error(409, "not_configured");
    };
    let pairing = owners.pairing().map(|pairing| PairingBody {
        code: pairing.code.as_str().into(),
        expires_in: pairing.expires_in.as_secs(),
    });
    to_json(
        200,
        &OwnersBody {
            owners: owners.owners(),
            pairing,
            ignored: owners.ignored(),
        },
    )
}

async fn owners_command<C: ChannelControl>(channel: &C, body: &[u8]) -> HttpResponse {
    let Some(owners) = channel.owners() else {
        return error(409, "not_configured");
    };
    match serde_json::from_slice(body) {
        Ok(OwnersCommand::Remove(id)) => match owners.remove(&id).await {
            Ok(_removed) => HttpResponse::new(204, JSON_CONTENT_TYPE, Vec::new()),
            Err(storage_error) => {
                log::error!("failed to store an owner removal: {storage_error}");
                error(500, "storage")
            }
        },
        Ok(OwnersCommand::Rotate(true)) => match owners.rotate() {
            Ok(()) => HttpResponse::new(204, JSON_CONTENT_TYPE, Vec::new()),
            Err(_unavailable) => error(503, "entropy_unavailable"),
        },
        Ok(OwnersCommand::Rotate(false)) | Err(_) => error(400, "invalid_request"),
    }
}

async fn owners_request<C: ChannelControl>(channel: &C, request: &HttpRequest) -> HttpResponse {
    match request.method() {
        HttpMethod::Get => list_owners(channel),
        HttpMethod::Post => owners_command(channel, request.body()).await,
        _ => error(405, "method_not_allowed"),
    }
}

impl<C: ChannelControl> HttpEndpoint for OwnersEndpoint<C> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move { owners_request(self.0.as_ref(), &request).await })
    }
}

/// Every route of a channel under its configuration path `base`, for one
/// prefix registration (`WebServer::serve_http_prefix(base, …)`):
///
/// - `base` itself goes to the channel Plugin's own `config` endpoint, which
///   takes changes only;
/// - `GET base/status` answers [`status_response`] (405 for other methods);
/// - `base/mode` is [`ModeEndpoint`];
/// - `base/owners` is [`OwnersEndpoint`];
/// - any other path below `base` answers 404 `not_found`.
///
/// Routes registered for exact paths below `base` (such as WeChat's
/// `/login`) still take precedence over it.
pub struct ChannelEndpoint<C, Config> {
    channel: Rc<C>,
    base: &'static str,
    config: Config,
}

impl<C, Config> ChannelEndpoint<C, Config> {
    /// Serves `channel`'s routes under `base`, with `config` on `base`.
    #[must_use]
    pub const fn new(channel: Rc<C>, base: &'static str, config: Config) -> Self {
        Self {
            channel,
            base,
            config,
        }
    }
}

impl<C: ChannelControl, Config: HttpEndpoint> HttpEndpoint for ChannelEndpoint<C, Config> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        let channel = self.channel.as_ref();
        match request.path().strip_prefix(self.base) {
            Some("") => self.config.handle(request),
            Some("/status") => Box::pin(async move {
                match request.method() {
                    HttpMethod::Get => status_response(channel),
                    _ => error(405, "method_not_allowed"),
                }
            }),
            Some("/mode") => Box::pin(async move { mode_request(channel, &request).await }),
            Some("/owners") => Box::pin(async move { owners_request(channel, &request).await }),
            _ => Box::pin(async { error(404, "not_found") }),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

fn error(status: u16, error: &'static str) -> HttpResponse {
    to_json(status, &ErrorBody { error })
}

fn to_json(status: u16, body: &impl Serialize) -> HttpResponse {
    let body = serde_json::to_vec(body).unwrap_or_else(|_error| Vec::from(&b"{}"[..]));
    HttpResponse::new(status, JSON_CONTENT_TYPE, body)
}
