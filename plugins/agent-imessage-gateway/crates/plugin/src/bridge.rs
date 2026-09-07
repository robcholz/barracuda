use alloc::{rc::Rc, vec::Vec};
use barracuda_plugin::manager::{
    PluginEntryIterator as _, PluginReadTransaction as _, PluginStorage, StorageError,
};
use barracuda_workflow_plugin::{
    WorkflowActionRegistration, WorkflowActionRegistry, WorkflowActionRegistryError,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

use crate::{
    state::{BridgeBook, PersistedRoute},
    to_agent::ToAgentAction,
    to_gateway::ToGatewayAction,
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

/// Persistent route state behind the two iMessage bridge Workflow Actions.
pub(crate) struct ImessageBridge<Storage> {
    control: BridgeControl<Storage>,
}

impl<Storage> ImessageBridge<Storage>
where
    Storage: PluginStorage,
{
    /// Restores persisted route mappings and constructs the bridge.
    pub(crate) async fn load(storage: Storage) -> Result<Self, ImessageBridgeStorageError> {
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

    /// Registers both stable Workflow Action addresses until the returned guards are dropped.
    pub(crate) fn register_actions(
        &self,
        actions: &WorkflowActionRegistry,
    ) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
        Ok(alloc::vec![
            actions.add_action(ToAgentAction::new(self.control.clone()))?,
            actions.add_action(ToGatewayAction::new(self.control.clone()))?,
        ])
    }
}
