//! Typed outbound HTTP capability and Workflow Action.

#![no_std]

extern crate alloc;

mod http;
mod workflow;

use alloc::rc::Rc;

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_workflow_plugin::WorkflowActionRegistry;
use http_client::ClientFactory;

pub use http::{Http, HttpError, HttpHeader, HttpMethod, HttpRequest, HttpResponse};

/// Plugin publishing outbound HTTP and registering its Workflow Action.
#[barracuda_plugin::macros::plugin]
pub struct HttpPlugin {
    clients: ClientFactory<'static>,
}

impl HttpPlugin {
    /// Creates the HTTP Plugin from System's shared client factory.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for HttpPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let http = Rc::new(Http::try_new(self.clients.clone()).map_err(PluginError::registration)?);
        let registration = workflow::register_action(&actions, Rc::clone(&http))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        context.provide(http)
    }
}
