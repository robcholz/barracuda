//! Outbound HTTP: the shared client factory, the typed request capability,
//! and the `http.request` Workflow Action.

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

/// Plugin owning outbound HTTP: the TLS engine, the connection pool, and the
/// capabilities and Workflow Action built on them.
#[barracuda_plugin::macros::plugin]
pub struct HttpPlugin {
    clients: ClientFactory<'static>,
}

impl HttpPlugin {
    /// Starts TLS from the Platform's entropy and the connection pool on its
    /// IP stack. Without TLS, `https://` requests fail rather than fall back
    /// to plaintext.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        let tls = match barracuda_tls::Tls::new(context.entropy.clone()) {
            Ok(tls) => {
                log::info!("initialized TLS");
                Some(tls)
            }
            Err(error) => {
                log::warn!("HTTPS is unavailable: {error}");
                None
            }
        };
        Self {
            clients: ClientFactory::new(context.ip_stack, tls),
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
        let http = Rc::new(Http::new(self.clients.clone()));
        let registration = workflow::register_action(&actions, Rc::clone(&http))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        context.provide(Rc::new(self.clients.clone()))?;
        context.provide(http)
    }
}
