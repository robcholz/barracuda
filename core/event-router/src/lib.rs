//! Event Router composition of Component routing and Workflow execution.

#![no_std]

extern crate alloc;

mod workflow;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_workflow::integration::WorkflowRuntimeView;

use workflow::WorkflowComponent;

pub use barracuda_fs::{FileSystem, FsError, FsFile};
pub use barracuda_router::{
    CleanupError, Component, ComponentCleanupFailure, ComponentError, ComponentFuture, ComponentId,
    ComponentResult, LoadError, RegisterContext, Router, RouterError, RunContext, UnloadError,
    UnregisterContext,
};
pub use barracuda_rpc::{
    rpc_dynamic, rpc_message, Dynamic, JsonCodec, RpcAddress, RpcAddressError, RpcCallId,
    RpcCardinality, RpcClient, RpcContext, RpcEndpointId, RpcError, RpcFrame, RpcGroup,
    RpcGroupError, RpcHandler, RpcHandlerFuture, RpcHandlerInput, RpcHandlerOutput, RpcInputMode,
    RpcLaneStorage, RpcMessage, RpcMethod, RpcMethodInfo, RpcMulticastBranch, RpcOutputMode,
    RpcPayloadFrame, RpcPayloadReader, RpcPayloadWriteFrame, RpcPayloadWriter, RpcRegistration,
    RpcRegistry, RpcResult, RpcStream, RpcUnaryCall, RpcWire, Streaming, Unary, WireField,
    WireSupport,
};
/// Host-side JSON Schema bake pipeline, surfaced through the facade for
/// `build.rs` and wire crates. Requires the `schema` feature and never ships to
/// the device.
#[cfg(feature = "schema")]
pub use barracuda_rpc_schema::{bake_all, register, SchemaEntry};
pub use barracuda_workflow::{
    validate_definition, EmitError, EmitRejection, Event, EventEmitter, EventId, EventIdError,
    EventInputMode, Rule, RuleError, Topic, TopicError, WorkflowClient, WorkflowControlError,
    WorkflowControlRejection, WorkflowDefinition, WorkflowDefinitionError, WorkflowExecutionError,
    WorkflowFailure, WorkflowId, WorkflowIdError, WorkflowInfo, WorkflowLoadError,
    WorkflowUnloadError, TOPIC_MAX_BYTES,
};

/// Failure while constructing Event Router and restoring durable Workflows.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EventRouterCreateError {
    /// The configured Workflow persistence directory is empty.
    #[error("Workflow persistence directory cannot be empty")]
    InvalidPersistenceDirectory,
    /// A filesystem operation failed while initializing or restoring Workflows.
    #[error(transparent)]
    Persistence(#[from] FsError),
    /// One persisted file did not contain a valid Workflow JSON document.
    #[error("invalid persisted Workflow {path}: {rejection:?}")]
    InvalidPersistedWorkflow {
        /// Persistence path containing invalid data.
        path: alloc::string::String,
        /// Workflow validation failure.
        rejection: WorkflowControlRejection,
    },
    /// A persisted file name did not match its JSON Workflow ID.
    #[error("persisted Workflow path does not match its ID: {0}")]
    MismatchedPersistedWorkflow(alloc::string::String),
    /// A Component lifecycle failure prevented Event Router construction.
    #[error(transparent)]
    Component(#[from] LoadError),
}

/// Event routing composition of a [`Router`] and its internal Workflow Runtime.
///
/// Construction wraps the Workflow Runtime in Event Router's private Component
/// adapter and loads that adapter first. It installs the unique `internal.emit`
/// endpoint. Additional Components use the same lifecycle and cooperative
/// polling model as the underlying Router.
pub struct EventRouter<const N: usize, const M: usize, const Q: usize> {
    router: Router<N, M, Q>,
    workflow: WorkflowRuntimeView,
}

impl<const N: usize, const M: usize, const Q: usize> EventRouter<N, M, Q> {
    /// Creates an Event Router backed by a statically dispatched filesystem.
    ///
    /// Persisted Workflow JSON is restored from `persistence_directory` before
    /// the internal Workflow Component is registered.
    ///
    /// # Errors
    ///
    /// Returns an error when the persistence directory cannot be initialized,
    /// a persisted Workflow is invalid, or the internal Component cannot be
    /// loaded.
    pub fn new<Filesystem>(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        filesystem: Filesystem,
        persistence_directory: impl Into<alloc::string::String>,
    ) -> Result<Self, EventRouterCreateError>
    where
        Filesystem: FileSystem,
    {
        let mut router = Router::new(lanes);
        let (workflow_component, workflow) =
            WorkflowComponent::new(filesystem, persistence_directory.into())?;
        router.load(Box::new(workflow_component))?;
        Ok(Self { router, workflow })
    }

    /// Returns an immutable snapshot of Workflow definitions and execution state.
    #[must_use]
    pub fn workflow_info(&self) -> WorkflowInfo {
        self.workflow.info()
    }

    /// Returns a snapshot of loaded Workflow definitions in load order.
    #[must_use]
    pub fn workflow_definitions(&self) -> Vec<WorkflowDefinition> {
        self.workflow.definitions()
    }

    /// Registers and loads one additional Component.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`Router::load`].
    pub fn load(&mut self, component: Box<dyn Component<M>>) -> Result<ComponentId, LoadError> {
        self.router.load(component)
    }

    /// Cancels, unregisters, and removes one additional Component.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`Router::unload`].
    pub fn unload(&mut self, id: ComponentId) -> Result<(), UnloadError> {
        self.router.unload(id)
    }
}

impl<const N: usize, const M: usize, const Q: usize> Future for EventRouter<N, M, Q> {
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use barracuda_platform_test::MemFs;
    use core::cell::Cell;
    use core::future::pending;

    use super::{
        Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext, RpcLaneStorage,
        RunContext, UnregisterContext,
    };

    const FRAME_SIZE: usize = 64;

    struct PendingComponent {
        registered: Rc<Cell<bool>>,
    }

    impl Component<FRAME_SIZE> for PendingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> ComponentResult<()> {
            self.registered.set(true);
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
            Ok(())
        }
    }

    #[test]
    fn event_router_composes_router_and_workflow_runtime() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let filesystem = MemFs::new();
        let mut event_router =
            EventRouter::new(lanes, filesystem, "workflows").expect("create Event Router");
        let registered = Rc::new(Cell::new(false));

        assert!(event_router.workflow_definitions().is_empty());
        let workflow = event_router.workflow_info();
        assert_eq!(workflow.completed_count, 0);
        assert_eq!(workflow.failed_count, 0);
        assert_eq!(workflow.cancelled_count, 0);
        assert!(workflow.last_failure.is_none());

        let component = event_router
            .load(Box::new(PendingComponent {
                registered: Rc::clone(&registered),
            }))
            .expect("load application Component");

        assert_eq!(component.value(), 2);
        assert!(registered.get());
    }
}
