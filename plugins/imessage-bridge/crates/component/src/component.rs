use alloc::{boxed::Box, rc::Rc};
use core::future::pending;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_plugin::manager::{
    PluginEntryIterator as _, PluginReadTransaction as _, PluginStorage, StorageError,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

use crate::{
    state::{BridgeBook, PersistedRoute},
    to_agent::{ToAgent, to_agent_handler},
    to_gateway::{ToGateway, to_gateway_handler},
};

pub(crate) struct BridgeShared<Storage> {
    pub(crate) book: Mutex<NoopRawMutex, BridgeBook>,
    pub(crate) storage: Storage,
}

#[derive(Clone)]
pub(crate) struct BridgeControl<Storage> {
    pub(crate) shared: Rc<BridgeShared<Storage>>,
}

/// Failure restoring the bridge's persistent route mappings.
#[derive(Debug, thiserror::Error)]
pub enum ImessageBridgeStorageError {
    /// Plugin-scoped storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A stored mapping is incompatible or conflicts with another mapping.
    #[error("iMessage bridge persistent state is invalid")]
    InvalidState,
}

/// Event Router Component exposing the two Workflow bridge RPCs.
pub struct ImessageBridgeComponent<Storage> {
    control: BridgeControl<Storage>,
}

impl<Storage> ImessageBridgeComponent<Storage>
where
    Storage: PluginStorage,
{
    /// Restores persisted route mappings and constructs the Component.
    pub async fn load(storage: Storage) -> Result<Self, ImessageBridgeStorageError> {
        let mut book = BridgeBook::new();
        let transaction = storage.read_transaction().await;
        let mut entries = transaction.entries().await?;
        while let Some(entry) = entries.next().await? {
            let record = entry.value::<PersistedRoute>()?;
            book.restore(entry.key(), record)
                .map_err(|_error| ImessageBridgeStorageError::InvalidState)?;
        }
        drop(entries);
        drop(transaction);
        Ok(Self {
            control: BridgeControl {
                shared: Rc::new(BridgeShared {
                    book: Mutex::new(book),
                    storage,
                }),
            },
        })
    }
}

impl<Storage, const M: usize> Component<M> for ImessageBridgeComponent<Storage>
where
    Storage: PluginStorage,
{
    fn name(&self) -> &'static str {
        "imessage-bridge"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_json::<ToAgent, _>("*", to_agent_handler(self.control.clone()))?;
        context.register_json::<ToGateway, _>("*", to_gateway_handler(self.control.clone()))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}
