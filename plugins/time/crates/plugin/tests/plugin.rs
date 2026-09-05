//! Time Plugin lifecycle tests.

#![allow(clippy::expect_used)]

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginId, PluginManager, PluginRegisterContext, PluginResult,
    PluginStartError,
};
use barracuda_time_component::{ClockError, UtcClock};
use barracuda_time_plugin::TimePlugin;
use embassy_executor::{Executor, Spawner};
use futures_lite::future::block_on;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

struct ClockConsumer {
    observed: Rc<RefCell<Option<Rc<UtcClock>>>>,
}

impl PluginDeclaration for ClockConsumer {
    const ID: &'static str = "time-test-consumer";
    const DEPENDS_ON: &'static [&'static str] = &["time"];
}

impl Plugin<512> for ClockConsumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, 512, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        *self.observed.borrow_mut() = Some(context.require::<UtcClock>("time")?);
        Ok(())
    }
}

#[test]
fn plugin_loads_the_time_component() {
    block_on(async {
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create database partition");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let mut router = EventRouter::new(lanes).await.expect("create router");
        let id = PluginId::try_from("time").expect("valid Plugin ID");
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        let plugin = TimePlugin::new(&mut context);

        assert_eq!(
            <TimePlugin as barracuda_plugin_manager::PluginDeclaration>::ID,
            "time"
        );
        assert!(<TimePlugin as barracuda_plugin_manager::PluginDeclaration>::DEPENDS_ON.is_empty());
        manager
            .register(&mut router, plugin)
            .expect("register Time Plugin");
        let observed = Rc::new(RefCell::new(None));
        manager
            .register(
                &mut router,
                ClockConsumer {
                    observed: Rc::clone(&observed),
                },
            )
            .expect("require UTC clock capability");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        assert_eq!(
            observed
                .borrow()
                .as_ref()
                .expect("UTC clock capability")
                .now(),
            Err(ClockError::Unsynchronized)
        );
        let error = manager
            .start(&mut router)
            .expect_err("Time task requires the System spawner");
        assert!(matches!(error, PluginStartError::Start(_)));
    });
}

#[embassy_executor::task]
async fn reload_time_plugin(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let partition = memory_partition(64 * 1024)
            .await
            .map_err(|error| error.to_string())?;
        let mut manager = PluginManager::open(partition)
            .await
            .map_err(|error| error.to_string())?;
        install_global_memory_vfs()
            .await
            .map_err(|error| error.to_string())?;
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let mut router = EventRouter::new(lanes)
            .await
            .map_err(|error| error.to_string())?;
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));

        manager.install_task_spawner(spawner);
        manager
            .register(&mut router, TimePlugin::new(&mut context))
            .map_err(|error| error.to_string())?;
        manager
            .start(&mut router)
            .map_err(|error| error.to_string())?;

        let id = PluginId::try_from("time").map_err(|error| error.to_string())?;
        manager
            .unload(&mut router, &id)
            .await
            .map_err(|error| error.to_string())?;

        let mut reloaded_context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        manager
            .register(&mut router, TimePlugin::new(&mut reloaded_context))
            .map_err(|error| error.to_string())?;
        manager
            .start(&mut router)
            .map_err(|error| error.to_string())?;
        manager
            .unload(&mut router, &id)
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    let _result = completed.send(result);
}

#[test]
fn plugin_task_can_be_unloaded_and_reloaded_in_its_fixed_pool() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(reload_time_plugin(spawner, completed))
                .expect("spawn Time Plugin test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("Time Plugin startup timed out")
        .expect("Time Plugin startup failed");
}
