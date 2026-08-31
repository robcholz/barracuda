//! Plugin entry point for the RTC-authoritative Scheduler Component.

#![no_std]

extern crate alloc;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};
use barracuda_scheduler_component::{SchedulerComponent, SchedulerConfig};

const SCHEDULE_CAPACITY: usize = 16;
const MAX_RECHECK_MILLIS: u64 = 1_000;

/// Plugin that owns the RTC-authoritative Scheduler Component.
pub struct SchedulerPlugin;

impl SchedulerPlugin {
    /// Creates the Scheduler Plugin from the shared construction context.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for SchedulerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context
            .event_router
            .load(SchedulerComponent::new(SchedulerConfig::new(
                SCHEDULE_CAPACITY,
                MAX_RECHECK_MILLIS,
            )))?;
        Ok(())
    }
}
