//! QQ channel state shared by the endpoints, the portal status, and the
//! receive loop.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};

use barracuda_imessage_gateway_channel::{
    load_mode, receive_runtime, store_mode, sync_receive, ChannelControl, ChannelMode, ModeError,
    ModeFuture, OnDemand, Owners, OwnersError, PairingEntropy, ReceiveChannel, ReceiveControl,
    ReceiveRuntime, ReceiveSlotSource, ReceiveTiming,
};
use barracuda_imessage_gateway_plugin::{
    GatewayError, IMessageGateway, MessageChannel, MessageChannelRegistration,
};
use barracuda_plugin::api::{Entropy, SharedEntropy};
use barracuda_plugin::manager::{PluginError, PluginResult, PluginStorage};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::embedded_nal_async::{Dns, TcpConnect};
use http_client::ClientFactory;
use qq::QQ;

use crate::receive::{GatewayState, SESSION_STORAGE_KEY};
use crate::{load_configuration, ConfigRequest, CONFIGURATION_STORAGE_KEY};

/// A stored configuration and the provider built from it.
pub(crate) struct Settings<T: 'static, D: 'static> {
    pub(crate) config: ConfigRequest,
    /// Sends, and fetches tokens and the gateway URL, through the shared
    /// request pool, never a receive slot.
    pub(crate) sender: Rc<QQ<'static, T, D>>,
}

/// Why a configuration was not applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigureError {
    Storage,
    Registration,
}

/// The QQ channel: configuration, mode, Gateway registration, owners, the
/// gateway session, and receive state.
pub(crate) struct QQChannel<Storage, Slots: ReceiveSlotSource, T: 'static, D: 'static> {
    pub(crate) gateway: Rc<IMessageGateway>,
    pub(crate) storage: Storage,
    pub(crate) http_clients: ClientFactory<'static, T, D>,
    settings: RefCell<Option<Rc<Settings<T, D>>>>,
    mode: Cell<ChannelMode>,
    /// Present exactly while configured and the mode registers.
    registration: RefCell<Option<MessageChannelRegistration>>,
    receive: Rc<ReceiveControl<Slots>>,
    /// Loaded once the channel is configured.
    owners: OnDemand<Owners<Storage>>,
    /// WebSocket keys and masks.
    pub(crate) entropy: SharedEntropy,
    /// The gateway session and recent messages, loaded by the first receive
    /// session.
    pub(crate) receiving: OnDemand<GatewayState>,
    /// Serializes configuration and mode changes.
    changes: Mutex<NoopRawMutex, ()>,
}

/// The channel and the receive runtime its task owns.
pub(crate) type Built<Storage, Slots, T, D> = (Rc<QQChannel<Storage, Slots, T, D>>, ReceiveRuntime);

/// Loads the channel from storage, registers it with the Gateway when its mode
/// asks for it, and builds the receive runtime the Plugin's task owns.
pub(crate) fn build_channel<Storage, Slots, T, D>(
    storage: Storage,
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static, T, D>,
    slots: Slots,
    entropy: SharedEntropy,
    timing: ReceiveTiming,
) -> PluginResult<Built<Storage, Slots, T, D>>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
    QQChannel<Storage, Slots, T, D>: ReceiveChannel<Slots::Lease>,
{
    let config = embassy_futures::block_on(load_configuration(&storage))?;
    let mode =
        embassy_futures::block_on(load_mode(&storage, config.is_some(), ChannelMode::legacy()))?;
    let settings = config.map(|config| {
        Rc::new(Settings {
            sender: Rc::new(QQ::new(http_clients.clone(), config.clone().into())),
            config,
        })
    });
    let receive = Rc::new(ReceiveControl::new(slots));
    let channel = Rc::new(QQChannel {
        gateway,
        storage,
        http_clients,
        settings: RefCell::new(settings),
        mode: Cell::new(mode),
        registration: RefCell::new(None),
        receive: Rc::clone(&receive),
        owners: OnDemand::new(),
        entropy,
        receiving: OnDemand::new(),
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
        log::warn!("QQ waits for a free receive slot");
    }
    Ok((channel, runtime))
}

impl<Storage, Slots, T, D> QQChannel<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// The current configuration and its provider.
    pub(crate) fn settings(&self) -> Option<Rc<Settings<T, D>>> {
        self.settings.borrow().clone()
    }

    /// Loads the owner book, once; a configured channel needs it.
    async fn load_owners(&self) -> Result<&Owners<Storage>, OwnersError> {
        self.owners
            .get_or_load(|| {
                Owners::load(
                    self.storage.clone(),
                    PairingEntropy::new(self.entropy.clone()),
                )
            })
            .await
    }

    /// Makes `id` an owner without a pairing code: the person who bound the
    /// bot by QR code. Returns whether it was added.
    pub(crate) async fn add_owner(&self, id: &str) -> Result<bool, OwnersError> {
        self.load_owners().await?.add(id, None).await
    }

    /// Sixteen bytes from the Platform entropy, or zeros without one.
    pub(crate) fn random_key(&self) -> [u8; 16] {
        let mut key = [0_u8; 16];
        if self.entropy.fill(&mut key).is_err() {
            log::warn!("no entropy for the QQ WebSocket key");
        }
        key
    }

    /// Registers or unregisters with the Gateway so that a registration exists
    /// exactly while configured and `mode` registers.
    fn register_for(&self, mode: ChannelMode) -> Result<(), GatewayError> {
        let wanted = self.settings().filter(|_settings| mode.registers());
        let mut registration = self.registration.borrow_mut();
        match (wanted, registration.is_some()) {
            (Some(settings), false) => {
                let sender: Rc<dyn MessageChannel> = settings.sender.clone();
                *registration = Some(self.gateway.register(sender)?);
            }
            (None, true) => {
                registration.take();
            }
            _ => {}
        }
        Ok(())
    }

    /// Stores and applies a verified configuration, then restarts receiving
    /// with it. `sender` already holds the token the verification fetched.
    ///
    /// When the Gateway rejects the channel, the previous stored
    /// configuration is restored and no channel stays registered.
    pub(crate) async fn configure(
        &self,
        config: ConfigRequest,
        sender: Rc<QQ<'static, T, D>>,
    ) -> Result<(), ConfigureError> {
        let _change = self.changes.lock().await;
        self.load_owners().await.map_err(|error| {
            log::error!("failed to read the QQ owners: {error}");
            ConfigureError::Storage
        })?;
        let previous = self
            .storage
            .get_bytes(CONFIGURATION_STORAGE_KEY)
            .await
            .map_err(|error| {
                log::error!("failed to read the previous QQ gateway configuration: {error}");
                ConfigureError::Storage
            })?;
        let bytes = serde_json::to_vec(&config).map_err(|_error| ConfigureError::Storage)?;
        self.storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
            .map_err(|error| {
                log::error!("failed to persist QQ gateway configuration: {error}");
                ConfigureError::Storage
            })?;
        let same_bot = self.settings().is_some_and(|settings| {
            settings.config.app_id == config.app_id && settings.config.api_base == config.api_base
        });
        self.registration.take();
        self.settings
            .replace(Some(Rc::new(Settings { config, sender })));
        if let Err(error) = self.register_for(self.mode.get()) {
            log::warn!("rejected QQ gateway configuration: {error}");
            self.settings.replace(None);
            let _no_slot = sync_receive(self);
            let restored = match previous.as_deref() {
                Some(previous) => self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await,
                None => self.storage.delete(CONFIGURATION_STORAGE_KEY).await,
            };
            if let Err(error) = restored {
                log::error!("failed to roll back rejected QQ gateway configuration: {error}");
                return Err(ConfigureError::Storage);
            }
            return Err(ConfigureError::Registration);
        }
        if !same_bot {
            // Another bot's session and gateway do not apply.
            let cleared = match self.receiving.get() {
                Some(state) => state.clear(&self.storage).await,
                None => self.storage.delete(SESSION_STORAGE_KEY).await,
            };
            if let Err(error) = cleared {
                log::warn!("failed to clear the QQ gateway session: {error}");
            }
        }
        if sync_receive(self).is_err() {
            log::warn!("QQ waits for a free receive slot");
        }
        self.receive.restart();
        log::info!("configured QQ gateway provider");
        Ok(())
    }
}

impl<Storage, Slots, T, D> ChannelControl for QQChannel<Storage, Slots, T, D>
where
    Storage: PluginStorage,
    Slots: ReceiveSlotSource,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    type Storage = Storage;
    type Slots = Slots;

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
                log::error!("failed to store the QQ mode: {error}");
                ModeError::Storage
            })?;
            if let Err(error) = self.register_for(mode) {
                log::warn!("the Gateway rejected the QQ channel: {error}");
                if let Err(error) = store_mode(&self.storage, previous).await {
                    log::error!("failed to restore the QQ mode: {error}");
                }
                return Err(ModeError::Registration);
            }
            self.mode.set(mode);
            Ok(())
        })
    }

    fn receive(&self) -> &ReceiveControl<Slots> {
        &self.receive
    }

    fn owners(&self) -> Option<&Owners<Storage>> {
        self.owners.get()
    }
}
