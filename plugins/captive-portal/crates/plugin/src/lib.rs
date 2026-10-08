//! Portal aggregation above WebServer.
#![no_std]
extern crate alloc;

mod portal;
mod resources;

use alloc::rc::Rc;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginRegisterContext,
    PluginRequirements, PluginResult,
};
use barracuda_webserver_plugin::{HttpProvider, HttpResponse, WebServer};

/// A leaf resource provider; it receives a validated, relative asset path.
pub use barracuda_webserver_plugin::HttpProvider as AssetsProvider;
pub use portal::{CaptivePortal, PortalError, WebEntry, WebEntryRegistration, WebGroup, WebText};
pub use resources::ResourceFiles;

/// Plugin owning the portal resource namespace and aggregate web route.
#[barracuda_plugin::macros::plugin]
pub struct CaptivePortalPlugin;

impl CaptivePortalPlugin {
    /// Constructs the portal without selecting a frontend framework.
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for CaptivePortalPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let webserver = context.require::<WebServer>(Self::DEPENDS_ON[0])?;
        let portal = Rc::new(CaptivePortal::new(ResourceFiles::from(
            context.filesystem()?.clone(),
        )));
        let route = webserver
            .serve("/portal/*", PortalRoute(portal.clone()))
            .map_err(PluginError::registration)?;
        context.retain(route);
        context.provide(portal)?;
        Ok(())
    }
}

struct PortalRoute(Rc<CaptivePortal>);
impl HttpProvider for PortalRoute {
    async fn serve(&self, path: &str) -> HttpResponse {
        self.0.serve(path).await
    }
}
