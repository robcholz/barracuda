//! Persistent Scheduler capability and Workflow Event producer.

#![no_std]

extern crate alloc;

mod scheduler;
mod state;

use alloc::rc::Rc;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_time_plugin::UtcClock;
use barracuda_workflow_plugin::WorkflowService;
use embassy_futures::select::select;

pub use scheduler::{
    CancelRequest, ScheduleAccepted, ScheduleCancelled, ScheduleError, ScheduleRequest,
    ScheduleTrigger, Scheduler, SchedulerConfig, SchedulerRunError, SchedulerStorageError,
    SchedulerTriggered,
};

const MAX_RECHECK_MILLIS: u64 = 1_000;

/// Plugin that owns, runs, and publishes the Scheduler capability.
#[barracuda_plugin::macros::plugin]
pub struct SchedulerPlugin {
    runtime: Option<SchedulerRuntime>,
}

struct SchedulerRuntime {
    scheduler: Rc<Scheduler>,
    workflow: Rc<WorkflowService>,
}

impl SchedulerPlugin {
    /// Creates the Scheduler Plugin from the shared construction context.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for SchedulerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let clock = context.require::<UtcClock>("time")?;
        let workflow = context.require::<WorkflowService>("workflow")?;
        let scheduler = Rc::new(
            embassy_futures::block_on(Scheduler::load(
                SchedulerConfig::new(MAX_RECHECK_MILLIS),
                clock,
                context.storage().clone(),
            ))
            .map_err(PluginError::registration)?,
        );
        context.provide(Rc::clone(&scheduler))?;
        self.runtime = Some(SchedulerRuntime {
            scheduler,
            workflow,
        });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(SchedulerRuntimeUnavailable))?;
        let task = scheduler_task(runtime.scheduler, runtime.workflow, context.task_token())
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn scheduler_task(
    scheduler: Rc<Scheduler>,
    workflow: Rc<WorkflowService>,
    cancellation: PluginTaskToken,
) {
    let running = async move {
        if let Err(error) = scheduler.run(&workflow).await {
            log::error!("Scheduler runtime stopped: {error}");
        }
    };
    let _completed = select(cancellation.cancelled(), running).await;
    log::info!("stopped Scheduler runtime task");
}

#[derive(Debug, thiserror::Error)]
#[error("Scheduler runtime was not prepared during Plugin registration")]
struct SchedulerRuntimeUnavailable;
