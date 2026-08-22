//! Host composition: initialize the Event Router and load the framework
//! Components that make up a Barracuda host.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, RuntimeService};
use barracuda_event_router::{
    Component, ComponentId, EventRouter, EventRouterCreateError, FileSystem, LoadError,
    RpcLaneStorage, RouterError, WorkflowDefinition,
};
use barracuda_gateway_agent_adapter::component::GatewayAgentAdapter;
use barracuda_message_gateway_component::component::{GatewayComponent, GatewayIngress};
use barracuda_net::{Dns, TcpConnect};
use gateway::MessageGateway;

/// Host composition that owns the Event Router and its loaded Components.
///
/// The Router is polled as a [`Future`]; loaded Components drive their
/// long-lived futures cooperatively inside it.
pub struct Host<const N: usize, const M: usize, const Q: usize> {
    router: EventRouter<N, M, Q>,
}

impl<const N: usize, const M: usize, const Q: usize> Host<N, M, Q> {
    /// Creates an Event Router and loads its internal Workflow Runtime.
    ///
    /// # Errors
    ///
    /// Returns [`EventRouterCreateError`] when the persistence directory cannot
    /// be initialized or a persisted Workflow is invalid.
    pub fn new<Filesystem>(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        filesystem: &'static Filesystem,
        persistence_directory: impl Into<String>,
    ) -> Result<Self, EventRouterCreateError>
    where
        Filesystem: FileSystem,
    {
        Ok(Self {
            router: EventRouter::new(lanes, filesystem, persistence_directory)?,
        })
    }

    /// Loads one additional Component.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`EventRouter::load`].
    pub fn load<C>(&mut self, component: C) -> Result<ComponentId, LoadError>
    where
        C: Component<M> + 'static,
    {
        self.router.load(Box::new(component))
    }

    /// Loads the Agent Component around an existing runtime.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`EventRouter::load`].
    pub fn load_agent<Filesystem, Http>(
        &mut self,
        runtime: AgentRuntime<Filesystem, Http>,
        service: RuntimeService<Filesystem, Http>,
    ) -> Result<ComponentId, LoadError>
    where
        Filesystem: FileSystem + 'static,
        Http: TcpConnect + Dns + 'static,
    {
        self.load(AgentComponent::new(runtime, service))
    }

    /// Loads the Gateway–Agent Adapter Component.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`EventRouter::load`].
    pub fn load_gateway_agent_adapter(&mut self) -> Result<ComponentId, LoadError> {
        self.load(GatewayAgentAdapter::default())
    }

    /// Loads the Message Gateway Component and returns its inbound producer.
    ///
    /// # Errors
    ///
    /// Returns the same lifecycle failures as [`EventRouter::load`].
    pub fn load_message_gateway(
        &mut self,
        gateway: MessageGateway,
        ingress_capacity: usize,
    ) -> Result<(ComponentId, GatewayIngress), LoadError> {
        let (component, ingress) = GatewayComponent::new(gateway, ingress_capacity);
        let id = self.load(component)?;
        Ok((id, ingress))
    }

    /// Returns a snapshot of loaded Workflow definitions in load order.
    #[must_use]
    pub fn workflow_definitions(&self) -> Vec<WorkflowDefinition> {
        self.router.workflow_definitions()
    }

    /// Consumes the Host and returns the underlying Event Router.
    #[must_use]
    pub fn into_router(self) -> EventRouter<N, M, Q> {
        self.router
    }
}

impl<const N: usize, const M: usize, const Q: usize> Future for Host<N, M, Q> {
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use super::Host;
    use barracuda_event_router::{MemFs, RpcLaneStorage};
    use static_cell::ConstStaticCell;

    #[test]
    fn host_initializes_and_loads_the_adapter() {
        static LANES: ConstStaticCell<RpcLaneStorage<4, 512, 4>> =
            ConstStaticCell::new(RpcLaneStorage::new());
        let filesystem: &'static MemFs = Box::leak(Box::new(MemFs::new()));
        let mut host = Host::new(LANES.take(), filesystem, "workflows").expect("build host");
        host.load_gateway_agent_adapter().expect("load adapter");
    }
}
