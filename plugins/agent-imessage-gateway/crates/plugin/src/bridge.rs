use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    future::poll_fn,
    task::Poll,
};

use barracuda_plugin::manager::{
    PluginEntryIterator as _, PluginReadTransaction as _, PluginStorage, StorageError,
};
use barracuda_workflow_plugin::{
    WorkflowActionRegistration, WorkflowActionRegistry, WorkflowActionRegistryError,
};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex, mutex::Mutex, waitqueue::MultiWakerRegistration,
};

use crate::{
    sessions::{SessionAction, SessionReplies, SessionStore},
    state::{BridgeBook, PersistedRoute},
    to_agent::ToAgentAction,
    to_gateway::ToGatewayAction,
};

pub(crate) struct BridgeShared<Storage> {
    pub(crate) book: Mutex<NoopRawMutex, BridgeBook>,
    pub(crate) storage: Storage,
    /// Wakes inbound messages waiting on a route reservation.
    pub(crate) routes: RouteChanges,
}

/// Broadcast of route mapping and reservation changes in the [`BridgeBook`].
///
/// Read [`generation`](Self::generation) and call [`notify`](Self::notify)
/// while holding the book lock, so a waiter cannot miss a change made between
/// its resolve and its wait.
pub(crate) struct RouteChanges {
    generation: Cell<u32>,
    waiters: RefCell<MultiWakerRegistration<4>>,
}

impl RouteChanges {
    const fn new() -> Self {
        Self {
            generation: Cell::new(0),
            waiters: RefCell::new(MultiWakerRegistration::new()),
        }
    }

    pub(crate) fn generation(&self) -> u32 {
        self.generation.get()
    }

    pub(crate) fn notify(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.waiters.borrow_mut().wake();
    }

    /// Completes once the book changed after `generation` was read.
    pub(crate) async fn changed_since(&self, generation: u32) {
        poll_fn(|context| {
            if self.generation.get() == generation {
                self.waiters.borrow_mut().register(context.waker());
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }
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
                    routes: RouteChanges::new(),
                }),
            },
        })
    }

    /// Registers the bridge's stable Workflow Action addresses until the
    /// returned guards are dropped. Session commands reach the Agent's
    /// sessions through `store` and answer through `replies`.
    pub(crate) fn register_actions(
        &self,
        actions: &WorkflowActionRegistry,
        store: Rc<dyn SessionStore>,
        replies: Rc<dyn SessionReplies>,
    ) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
        Ok(alloc::vec![
            actions.add_action(ToAgentAction::new(self.control.clone()))?,
            actions.add_action(ToGatewayAction::new(self.control.clone()))?,
            actions.add_action(SessionAction::new(self.control.clone(), store, replies))?,
        ])
    }
}

#[cfg(test)]
mod tests;
