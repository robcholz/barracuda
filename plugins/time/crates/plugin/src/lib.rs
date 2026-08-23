//! Plugin entry point for the network-synchronized Time Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_net::{Dns, UdpStack};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};
use barracuda_time_component::{
    TimeComponent, TimeConfig,
    sntp::{SntpConfig, SntpSource},
};

const SNTP_SERVER: &str = "pool.ntp.org";
const MINIMUM_UNIX_SECONDS: u64 = 1_704_067_200;
const RETRY_DELAY_MILLIS: u64 = 30_000;
const RESYNC_INTERVAL_MILLIS: u64 = 3_600_000;
const MAX_HOLDOVER_MILLIS: u64 = 86_400_000;

/// Stable identity of the Time Plugin.
pub const PLUGIN_ID: &str = "time";

/// Plugin that owns the network-synchronized Time Component.
pub struct TimePlugin<Network> {
    network: Option<Network>,
}

impl<Network> TimePlugin<Network> {
    /// Creates the Time Plugin from the platform network capability.
    #[must_use]
    pub const fn new(network: Network) -> Self {
        Self {
            network: Some(network),
        }
    }
}

impl<Network, const M: usize> Plugin<M> for TimePlugin<Network>
where
    Network: Dns + UdpStack + 'static,
{
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            let network = self
                .network
                .take()
                .ok_or_else(|| PluginError::registration(AlreadyRegistered))?;
            let source =
                SntpSource::new(network, SntpConfig::new(SNTP_SERVER, MINIMUM_UNIX_SECONDS));
            let config = TimeConfig::new(
                RETRY_DELAY_MILLIS,
                RESYNC_INTERVAL_MILLIS,
                MAX_HOLDOVER_MILLIS,
            );
            context.load(TimeComponent::new(source, config))?;
            Ok(())
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Time Plugin has already registered its Component")]
struct AlreadyRegistered;
