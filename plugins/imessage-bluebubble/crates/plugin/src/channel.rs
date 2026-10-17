//! The channel's state: configuration, mode, Gateway registration, owners,
//! and receive control, behind the shared channel endpoints.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_plugin::{
    ChannelControl, ChannelMode, GatewayError, IMessageGateway, MessageChannel,
    MessageChannelRegistration, ModeError, ModeFuture, OnDemand, Owners, OwnersError,
    ReceiveControl, ReceiveRuntime, ReceiveTiming, UnlimitedSlots,
};
use barracuda_plugin::api::SharedEntropy;
use barracuda_plugin::manager::{PluginError, PluginResult, PluginStorage};
use barracuda_webserver_plugin::WEB_SERVER_PORT;
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{with_timeout, Duration};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::state::{ReceiveBook, HOOK_PATH};

/// Plugin storage key of the provider configuration.
pub(crate) const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Longest the device waits for the server to delete its webhook when
/// receiving stops.
const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration accepted by `POST /api/gateway/bluebubbles` and stored as is.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigRequest {
    pub(crate) server_url: String,
    pub(crate) password: String,
    #[serde(default = "default_true")]
    pub(crate) use_private_api: bool,
    #[serde(default = "default_stream_edit_min_delta_bytes")]
    pub(crate) stream_edit_min_delta_bytes: usize,
    #[serde(default = "default_stream_max_edits")]
    pub(crate) stream_max_edits: usize,
}

impl From<ConfigRequest> for BlueBubblesConfig {
    fn from(value: ConfigRequest) -> Self {
        BlueBubblesConfig {
            server_url: value.server_url,
            password: value.password,
            use_private_api: value.use_private_api,
            stream_edit_min_delta_bytes: value.stream_edit_min_delta_bytes,
            stream_max_edits: value.stream_max_edits,
        }
    }
}

const fn default_true() -> bool {
    true
}
const fn default_stream_edit_min_delta_bytes() -> usize {
    128
}
const fn default_stream_max_edits() -> usize {
    4
}

pub(crate) fn encode_configuration(config: &ConfigRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(config)
}

pub(crate) fn decode_configuration(bytes: &[u8]) -> Result<ConfigRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// The device's LAN IPv4 address as text, or `None` while unknown.
pub(crate) type LocalAddress = Box<dyn Fn() -> Option<String>>;

/// The BlueBubbles channel's state.
pub(crate) struct BlueBubblesChannel<Storage> {
    pub(crate) gateway: Rc<IMessageGateway>,
    pub(crate) storage: Storage,
    /// Mints the webhook secret.
    pub(crate) entropy: SharedEntropy,
    address: LocalAddress,
    provider: RefCell<Option<Rc<BlueBubbles>>>,
    registration: RefCell<Option<MessageChannelRegistration>>,
    mode: Cell<ChannelMode>,
    receive: Rc<ReceiveControl<UnlimitedSlots>>,
    /// Loaded once the channel is configured.
    owners: OnDemand<Owners<Storage>>,
    pub(crate) book: ReceiveBook,
    /// Serializes configuration and mode changes.
    changes: Mutex<NoopRawMutex, ()>,
}

/// The channel and the receive runtime its Plugin's task owns.
pub(crate) type BuiltChannel<Storage> = (Rc<BlueBubblesChannel<Storage>>, ReceiveRuntime);

/// Restores the stored configuration, mode, owners, and receive state,
/// registers the channel with the Gateway when configured and the mode
/// registers, and builds the receive runtime the Plugin's task owns.
pub(crate) fn build_channel<Storage: PluginStorage>(
    storage: Storage,
    gateway: Rc<IMessageGateway>,
    entropy: SharedEntropy,
    address: LocalAddress,
    timing: ReceiveTiming,
) -> PluginResult<BuiltChannel<Storage>> {
    let config = embassy_futures::block_on(storage.get_bytes(CONFIGURATION_STORAGE_KEY))?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)?;
    let mode = embassy_futures::block_on(gateway.load_mode(
        &storage,
        config.is_some(),
        ChannelMode::legacy(),
    ))?;
    let book = embassy_futures::block_on(ReceiveBook::load(&storage))?;
    let provider = config.map(|config| Rc::new(BlueBubbles::new(gateway.http(), config.into())));
    let receive = Rc::new(gateway.webhook_receive_control());
    let channel = Rc::new(BlueBubblesChannel {
        gateway,
        storage,
        entropy,
        address,
        provider: RefCell::new(provider),
        registration: RefCell::new(None),
        mode: Cell::new(mode),
        receive: Rc::clone(&receive),
        owners: OnDemand::new(),
        book,
        changes: Mutex::new(()),
    });
    if channel.configured() {
        embassy_futures::block_on(channel.load_owners()).map_err(PluginError::registration)?;
    }
    channel
        .sync_registration()
        .map_err(PluginError::registration)?;
    let runtime = channel
        .gateway
        .receive_runtime(receive, Rc::clone(&channel), timing);
    // Webhook receiving holds no slot, so this never reports `no_slot`.
    channel.gateway.sync_receive(&*channel).ok();
    Ok((channel, runtime))
}

impl<Storage: PluginStorage> BlueBubblesChannel<Storage> {
    /// Loads the owner book, once; a configured channel needs it.
    async fn load_owners(&self) -> Result<&Owners<Storage>, OwnersError> {
        self.owners
            .get_or_load(|| self.gateway.load_owners(self.storage.clone()))
            .await
    }

    /// The configured provider.
    pub(crate) fn provider(&self) -> Option<Rc<BlueBubbles>> {
        self.provider.borrow().clone()
    }

    /// The webhook URL for `secret`, or `None` while the device has no LAN
    /// address.
    pub(crate) fn hook_url(&self, secret: &str) -> Option<String> {
        let address = (self.address)()?;
        Some(alloc::format!(
            "http://{address}:{WEB_SERVER_PORT}{HOOK_PATH}/{secret}"
        ))
    }

    /// Registers the channel with the Gateway exactly when it is configured
    /// and the mode registers.
    fn sync_registration(&self) -> Result<(), GatewayError> {
        let wanted = self.mode.get().registers();
        let provider = self.provider();
        match provider {
            Some(provider) if wanted => {
                if self.registration.borrow().is_none() {
                    let channel: Rc<dyn MessageChannel> = provider;
                    let registration = self.gateway.register(channel)?;
                    self.registration.replace(Some(registration));
                }
            }
            _ => {
                self.registration.take();
            }
        }
        Ok(())
    }

    /// Applies a configuration from `POST /api/gateway/bluebubbles`.
    ///
    /// Stores it, replaces the provider, and re-registers the channel. When
    /// the Gateway rejects the channel, the previous stored configuration is
    /// restored and no channel stays registered.
    pub(crate) async fn configure(&self, config: ConfigRequest) -> Result<(), ConfigureError> {
        let _changing = self.changes.lock().await;
        self.load_owners().await.map_err(|error| {
            log::error!("failed to read the BlueBubbles owners: {error}");
            ConfigureError::Storage
        })?;
        let previous = self
            .storage
            .get_bytes(CONFIGURATION_STORAGE_KEY)
            .await
            .map_err(|error| {
                log::error!("failed to read the previous BlueBubbles configuration: {error}");
                ConfigureError::Storage
            })?;
        let bytes = encode_configuration(&config).map_err(|_error| ConfigureError::Storage)?;
        self.storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
            .map_err(|error| {
                log::error!("failed to persist BlueBubbles configuration: {error}");
                ConfigureError::Storage
            })?;
        let provider = Rc::new(BlueBubbles::new(self.gateway.http(), config.into()));
        let previous_provider = self.provider();
        if let Some(previous_provider) = previous_provider.filter(|previous_provider| {
            self.receive.is_enabled() && previous_provider.server_url() != provider.server_url()
        }) {
            // The old server keeps no webhook pointing at this device.
            self.receive.set_enabled(false).ok();
            self.book.clear_inbox();
            self.unregister_webhook(&previous_provider).await;
        }
        self.registration.take();
        self.provider.replace(Some(provider));
        if let Err(error) = self.sync_registration() {
            log::warn!("rejected BlueBubbles configuration: {error}");
            self.provider.replace(None);
            let restored = match previous.as_deref() {
                Some(previous) => self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await,
                None => self.storage.delete(CONFIGURATION_STORAGE_KEY).await,
            };
            if let Err(storage_error) = restored {
                log::error!(
                    "failed to roll back rejected BlueBubbles configuration: {storage_error}"
                );
                return Err(ConfigureError::Storage);
            }
            return Err(ConfigureError::Registration);
        }
        log::info!("configured BlueBubbles gateway provider");
        Ok(())
    }

    /// Deletes this device's webhook from `provider`'s server, best effort:
    /// a failure leaves the record so the next registration retires it.
    pub(crate) async fn unregister_webhook(&self, provider: &BlueBubbles) {
        let Some(mut hook) = self.book.hook() else {
            return;
        };
        let suffix = alloc::format!("{HOOK_PATH}/{}", hook.secret);
        let registered = hook.url.clone();
        let deleted = with_timeout(UNREGISTER_TIMEOUT, async {
            for entry in provider.list_webhooks().await? {
                if registered.as_deref() == Some(entry.url.as_str()) || entry.url.ends_with(&suffix)
                {
                    provider.delete_webhook(entry.id).await?;
                }
            }
            Ok::<(), barracuda_imessage_gateway_plugin::ChannelError>(())
        })
        .await;
        match deleted {
            Ok(Ok(())) => {
                if hook.url.take().is_some() {
                    if let Err(error) = self.book.store_hook(&self.storage, hook).await {
                        log::warn!("failed to store the BlueBubbles webhook record: {error}");
                    }
                }
                log::info!("deleted the BlueBubbles webhook");
            }
            Ok(Err(error)) => log::warn!("failed to delete the BlueBubbles webhook: {error}"),
            Err(_timeout) => log::warn!("timed out deleting the BlueBubbles webhook"),
        }
    }
}

/// Why a configuration was not applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigureError {
    /// Storage failed.
    Storage,
    /// The Gateway rejected the channel.
    Registration,
}

impl<Storage: PluginStorage> ChannelControl for BlueBubblesChannel<Storage> {
    type Storage = Storage;
    type Slots = UnlimitedSlots;

    fn configured(&self) -> bool {
        self.provider.borrow().is_some()
    }

    fn mode(&self) -> ChannelMode {
        self.mode.get()
    }

    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_> {
        Box::pin(async move {
            let _changing = self.changes.lock().await;
            let previous = self.mode.get();
            self.gateway
                .store_mode(&self.storage, mode)
                .await
                .map_err(|_error| ModeError::Storage)?;
            self.mode.set(mode);
            if let Err(error) = self.sync_registration() {
                log::warn!("rejected BlueBubbles channel registration: {error}");
                self.mode.set(previous);
                if self
                    .gateway
                    .store_mode(&self.storage, previous)
                    .await
                    .is_err()
                {
                    log::error!("failed to restore the BlueBubbles channel mode");
                }
                return Err(ModeError::Registration);
            }
            if previous.receives() && !mode.receives() {
                // Stop the session before deleting, so it cannot re-register.
                self.receive.set_enabled(false).ok();
                self.book.clear_inbox();
                if let Some(provider) = self.provider() {
                    self.unregister_webhook(&provider).await;
                }
            }
            Ok(())
        })
    }

    fn receive(&self) -> &ReceiveControl<UnlimitedSlots> {
        &self.receive
    }

    fn owners(&self) -> Option<&Owners<Storage>> {
        self.owners.get()
    }

    /// `webhook`: the lost and skipped deliveries while receiving.
    fn status_details(&self) -> Map<String, Value> {
        let mut details = Map::new();
        if self.mode().receives() {
            details.insert(
                "webhook".into(),
                json!({ "lost": self.book.lost(), "skipped": self.book.skipped() }),
            );
        }
        details
    }
}
