//! Plugin entry point for the RTC-authoritative Scheduler Component.

#![no_std]

extern crate alloc;

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_scheduler_component::{SchedulerComponent, SchedulerConfig};
use barracuda_time_plugin::UtcClock;

const MAX_RECHECK_MILLIS: u64 = 1_000;

/// Plugin that owns the RTC-authoritative Scheduler Component.
#[barracuda_plugin::macros::plugin]
pub struct SchedulerPlugin;

impl SchedulerPlugin {
    /// Creates the Scheduler Plugin from the shared construction context.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for SchedulerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let clock = context.require::<UtcClock>("time")?;
        let component = embassy_futures::block_on(SchedulerComponent::load(
            SchedulerConfig::new(MAX_RECHECK_MILLIS),
            clock,
            context.storage().clone(),
        ))
        .map_err(PluginError::registration)?;
        context.event_router.load(component)?;
        Ok(())
    }
}
