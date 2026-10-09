//! Telegram channel state shared by the endpoints, the portal status, and the
//! receive loop.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_channel::{
    load_mode, receive_runtime, store_mode, sync_receive, ChannelControl, ChannelMode, ModeError,
    ModeFuture, OnDemand, Owners, OwnersError, PairingEntropy, ReceiveControl, ReceiveRuntime,
    ReceiveTiming,
};
use barracuda_imessage_gateway_plugin::{
    GatewayError, IMessageGateway, MessageChannel, MessageChannelRegistration,
};
use barracuda_plugin::manager::{PluginError, PluginResult, PluginStorage};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::{ClientFactory, ReceiveSlots};
use serde_json::{json, Map, Value};
use telegram::{Telegram, TelegramConfig};

use crate::receive::{Cursor, TelegramSlots, CURSOR_STORAGE_KEY};
use crate::{decode_configuration, encode_configuration, ConfigRequest, CONFIGURATION_STORAGE_KEY};

/// Builds the sending provider for a configuration.
type Connect = Box<dyn Fn(TelegramConfig) -> Rc<dyn MessageChannel>>;

/// A stored configuration and the provider built from it.
pub(crate) struct Settings {
    pub(crate) config: ConfigRequest,
    /// Sends through the shared request pool, never a receive slot.
    pub(crate) sender: Rc<dyn MessageChannel>,
}

/// Why a configuration was not applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigureError {
    Storage,
    Registration,
}

/// The Telegram channel: configuration, mode, Gateway registration, owners,
/// and receive state.
pub(crate) struct TelegramChannel<Storage, C: 'static, D: 'static> {
    pub(crate) gateway: Rc<IMessageGateway>,
    pub(crate) storage: Storage,
    connect: Connect,
    settings: RefCell<Option<Rc<Settings>>>,
    mode: Cell<ChannelMode>,
    /// Present exactly while configured and the mode registers.
    registration: RefCell<Option<MessageChannelRegistration>>,
    receive: Rc<ReceiveControl<TelegramSlots<C, D>>>,
    /// Loaded once the channel is configured.
    owners: OnDemand<Owners<Storage>>,
    entropy: PairingEntropy,
    /// Loaded by the first receive session.
    pub(crate) cursor: OnDemand<Cursor>,
    /// Whether this entry into `send_receive` already deleted a webhook.
    webhook_cleared: Cell<bool>,
    /// Serializes configuration and mode changes.
    changes: Mutex<NoopRawMutex, ()>,
}

/// The channel and the receive runtime its Plugin's task owns.
pub(crate) type BuiltChannel<Storage, C, D> = (Rc<TelegramChannel<Storage, C, D>>, ReceiveRuntime);

/// Loads the channel from storage, registers it with the Gateway when its mode
/// asks for it, and builds the receive runtime the Plugin's task owns.
pub(crate) fn build_channel<Storage, T, C, D>(
    storage: Storage,
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static, T, D>,
    slots: ReceiveSlots<C, D>,
    entropy: PairingEntropy,
    timing: ReceiveTiming,
) -> PluginResult<BuiltChannel<Storage, C, D>>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    C: TcpConnect + 'static,
    D: Dns + 'static,
{
    let config = embassy_futures::block_on(storage.get_bytes(CONFIGURATION_STORAGE_KEY))?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)?;
    let mode =
        embassy_futures::block_on(load_mode(&storage, config.is_some(), ChannelMode::legacy()))?;
    let connect: Connect = Box::new(move |config| {
        let sender: Rc<dyn MessageChannel> = Rc::new(Telegram::new(http_clients.clone(), config));
        sender
    });
    let settings = config.map(|config| {
        Rc::new(Settings {
            sender: connect(config.clone().into()),
            config,
        })
    });
    let receive = Rc::new(ReceiveControl::new(TelegramSlots(slots)));
    let channel = Rc::new(TelegramChannel {
        gateway,
        storage,
        connect,
        settings: RefCell::new(settings),
        mode: Cell::new(mode),
        registration: RefCell::new(None),
        receive: Rc::clone(&receive),
        owners: OnDemand::new(),
        entropy,
        cursor: OnDemand::new(),
        webhook_cleared: Cell::new(false),
        changes: Mutex::new(()),
    });
    if channel.configured() {
        embassy_futures::block_on(channel.load_owners()).map_err(PluginError::registration)?;
    }
    channel
        .register_for(mode)
        .map_err(PluginError::registration)?;
    let runtime = receive_runtime(receive, Rc::clone(&channel), timing);
    if sync_receive(&*channel).is_err() {
        log::warn!("Telegram waits for a free receive slot");
    }
    Ok((channel, runtime))
}

impl<Storage: PluginStorage, C: 'static, D: 'static> TelegramChannel<Storage, C, D> {
    /// The current configuration and its provider.
    pub(crate) fn settings(&self) -> Option<Rc<Settings>> {
        self.settings.borrow().clone()
    }

    pub(crate) fn webhook_cleared(&self) -> bool {
        self.webhook_cleared.get()
    }

    pub(crate) fn mark_webhook_cleared(&self) {
        self.webhook_cleared.set(true);
    }

    /// Loads the owner book, once; a configured channel needs it.
    async fn load_owners(&self) -> Result<&Owners<Storage>, OwnersError> {
        self.owners
            .get_or_load(|| Owners::load(self.storage.clone(), self.entropy.clone()))
            .await
    }

    /// Registers or unregisters with the Gateway so that a registration exists
    /// exactly while configured and `mode` registers.
    fn register_for(&self, mode: ChannelMode) -> Result<(), GatewayError> {
        let wanted = self.settings().filter(|_settings| mode.registers());
        let mut registration = self.registration.borrow_mut();
        match (wanted, registration.is_some()) {
            (Some(settings), false) => {
                *registration = Some(self.gateway.register(Rc::clone(&settings.sender))?);
            }
            (None, true) => {
                registration.take();
            }
            _ => {}
        }
        Ok(())
    }

    /// Stores and applies a new configuration, then restarts receiving with it.
    ///
    /// When the Gateway rejects the channel, the previous stored
    /// configuration is restored and no channel stays registered.
    pub(crate) async fn configure(&self, config: ConfigRequest) -> Result<(), ConfigureError> {
        let _change = self.changes.lock().await;
        self.load_owners().await.map_err(|error| {
            log::error!("failed to read the Telegram owners: {error}");
            ConfigureError::Storage
        })?;
        let previous = self
            .storage
            .get_bytes(CONFIGURATION_STORAGE_KEY)
            .await
            .map_err(|error| {
                log::error!("failed to read the previous Telegram configuration: {error}");
                ConfigureError::Storage
            })?;
        let bytes = encode_configuration(&config).map_err(|_error| ConfigureError::Storage)?;
        self.storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
            .map_err(|error| {
                log::error!("failed to store the Telegram configuration: {error}");
                ConfigureError::Storage
            })?;
        let same_bot = self
            .settings()
            .is_some_and(|settings| settings.config.token == config.token);
        let settings = Rc::new(Settings {
            sender: (self.connect)(config.clone().into()),
            config,
        });
        self.registration.take();
        self.settings.replace(Some(settings));
        if let Err(error) = self.register_for(self.mode.get()) {
            log::warn!("rejected Telegram configuration: {error}");
            self.settings.replace(None);
            let _no_slot = sync_receive(self);
            let restored = match previous.as_deref() {
                Some(previous) => self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await,
                None => self.storage.delete(CONFIGURATION_STORAGE_KEY).await,
            };
            if let Err(error) = restored {
                log::error!("failed to roll back a rejected Telegram configuration: {error}");
                return Err(ConfigureError::Storage);
            }
            return Err(ConfigureError::Registration);
        }
        self.webhook_cleared.set(false);
        if !same_bot {
            // Another bot's update ids would hide this bot's updates.
            if let Some(cursor) = self.cursor.get() {
                cursor.clear();
            }
        }
        if sync_receive(self).is_err() {
            log::warn!("Telegram waits for a free receive slot");
        }
        self.receive.restart();
        if !same_bot {
            let forgotten = match self.cursor.get() {
                Some(cursor) => cursor.forget(&self.storage).await,
                None => self.storage.delete(CURSOR_STORAGE_KEY).await,
            };
            if let Err(error) = forgotten {
                log::warn!("failed to clear the Telegram receive cursor: {error}");
            }
        }
        log::info!("configured Telegram gateway provider");
        Ok(())
    }
}

impl<Storage: PluginStorage, C: 'static, D: 'static> ChannelControl
    for TelegramChannel<Storage, C, D>
{
    type Storage = Storage;
    type Slots = TelegramSlots<C, D>;

    fn configured(&self) -> bool {
        self.settings.borrow().is_some()
    }

    fn mode(&self) -> ChannelMode {
        self.mode.get()
    }

    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_> {
        Box::pin(async move {
            let _change = self.changes.lock().await;
            let previous = self.mode.get();
            store_mode(&self.storage, mode).await.map_err(|error| {
                log::error!("failed to store the Telegram mode: {error}");
                ModeError::Storage
            })?;
            if let Err(error) = self.register_for(mode) {
                log::warn!("the Gateway rejected the Telegram channel: {error}");
                if let Err(error) = store_mode(&self.storage, previous).await {
                    log::error!("failed to restore the Telegram mode: {error}");
                }
                return Err(ModeError::Registration);
            }
            if mode.receives() && !previous.receives() {
                self.webhook_cleared.set(false);
            }
            self.mode.set(mode);
            Ok(())
        })
    }

    fn receive(&self) -> &ReceiveControl<TelegramSlots<C, D>> {
        &self.receive
    }

    fn owners(&self) -> Option<&Owners<Storage>> {
        self.owners.get()
    }

    /// `config`: the bot's numeric ID, the part of the token before `:`; the
    /// rest of the token is never reported.
    fn status_details(&self) -> Map<String, Value> {
        let mut details = Map::new();
        if let Some(settings) = self.settings.borrow().as_ref() {
            let bot_id = settings.config.token.split_once(':').map(|(id, _)| id);
            details.insert("config".into(), json!({ "bot_id": bot_id }));
        }
        details
    }
}
