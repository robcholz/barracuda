//! Plugin entry point for the RTC-authoritative Scheduler Component.

#![no_std]

extern crate alloc;

use barracuda_plugin_manager::{Plugin, PluginContext, PluginResult};
use barracuda_scheduler_component::{SchedulerComponent, SchedulerConfig};
use barracuda_time_plugin::PLUGIN_ID as TIME_PLUGIN_ID;

/// Stable identity of the Scheduler Plugin.
pub const PLUGIN_ID: &str = "scheduler";

const SCHEDULE_CAPACITY: usize = 16;
const MAX_RECHECK_MILLIS: u64 = 1_000;

/// Plugin that owns the RTC-authoritative Scheduler Component.
pub struct SchedulerPlugin;

impl<const M: usize> Plugin<M> for SchedulerPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[TIME_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
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
