//! Scheduler Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin::api::{ClientFactory, PluginContext};
use barracuda_plugin::manager::{PluginId, PluginManager, PluginRegisterError};
use barracuda_scheduler_plugin::SchedulerPlugin;
use barracuda_time_plugin::TimePlugin;
use embassy_executor::{Executor, Spawner};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

#[embassy_executor::task]
async fn start_scheduler_with_time(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let partition = memory_partition(64 * 1024)
            .await
            .map_err(|error| error.to_string())?;
        let mut manager = PluginManager::open(partition)
            .await
            .map_err(|error| error.to_string())?;
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        install_global_memory_vfs()
            .await
            .map_err(|error| error.to_string())?;
        let mut router = EventRouter::new(lanes)
            .await
            .map_err(|error| error.to_string())?;
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        manager.install_task_spawner(spawner);

        assert_eq!(
            <SchedulerPlugin as barracuda_plugin::manager::PluginDeclaration>::ID,
            "scheduler"
        );
        assert_eq!(
            <SchedulerPlugin as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON,
            &["time"],
        );
        let error = manager
            .register(&mut router, SchedulerPlugin::new(&mut context))
            .expect_err("reject Scheduler before Time");
        assert!(
            matches!(error, PluginRegisterError::MissingDependency(id) if id.as_str() == "time")
        );

        manager
            .register(&mut router, TimePlugin::new(&mut context))
            .map_err(|error| error.to_string())?;
        manager
            .register(&mut router, SchedulerPlugin::new(&mut context))
            .map_err(|error| error.to_string())?;
        manager
            .start(&mut router)
            .map_err(|error| error.to_string())?;

        let id = PluginId::try_from("scheduler").expect("valid Plugin ID");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        manager
            .unload(&mut router, &id)
            .await
            .map_err(|error| error.to_string())?;
        let time_id =
            PluginId::try_from(<TimePlugin as barracuda_plugin::manager::PluginDeclaration>::ID)
                .expect("valid Time Plugin ID");
        manager
            .unload(&mut router, &time_id)
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    let _result = completed.send(result);
}

#[test]
fn plugin_requires_time_and_loads_the_scheduler_component() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(start_scheduler_with_time(spawner, completed))
                .expect("spawn Scheduler Plugin test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("Scheduler Plugin startup timed out")
        .expect("Scheduler Plugin startup failed");
}
