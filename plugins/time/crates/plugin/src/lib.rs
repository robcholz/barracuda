//! Plugin entry point for the network-synchronized Time Component.

#![no_std]

extern crate alloc;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};
use barracuda_time_component::{
    TimeComponent, TimeConfig,
    sntp::{SntpConfig, SntpSource},
};
use embassy_net::Stack;

const SNTP_SERVER: &str = "pool.ntp.org";
const MINIMUM_UNIX_SECONDS: u64 = 1_704_067_200;
const RETRY_DELAY_MILLIS: u64 = 30_000;
const RESYNC_INTERVAL_MILLIS: u64 = 3_600_000;
const MAX_HOLDOVER_MILLIS: u64 = 86_400_000;

/// Plugin that owns the network-synchronized Time Component.
pub struct TimePlugin {
    network: Stack<'static>,
}

impl TimePlugin {
    /// Creates the Plugin with the IP stack used by its SNTP source.
    #[must_use]
    pub const fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            network: context.ip_stack,
        }
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for TimePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let source = SntpSource::new(
            self.network,
            SntpConfig::new(SNTP_SERVER, MINIMUM_UNIX_SECONDS),
        );
        let config = TimeConfig::new(
            RETRY_DELAY_MILLIS,
            RESYNC_INTERVAL_MILLIS,
            MAX_HOLDOVER_MILLIS,
        );
        context
            .event_router
            .load(TimeComponent::new(source, config))?;
        Ok(())
    }
}
