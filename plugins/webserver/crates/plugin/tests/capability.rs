//! WebServer capability sharing integration test.

#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{
    Plugin, PluginManager, PluginRegisterContext, PluginResult, PluginStartError,
};
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use embassy_executor::{Executor, Spawner};
use futures_lite::future::block_on;

const FRAME_SIZE: usize = 64;
const EXECUTOR_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

fn plugin_context() -> PluginContext {
    let stack = never_embassy_stack();
    PluginContext::new(stack, ClientFactory::plaintext(stack))
}

struct Consumer {
    observed: Rc<RefCell<Option<Rc<WebServer>>>>,
}

impl Plugin<FRAME_SIZE> for Consumer {
    const DEPENDS_ON: &'static [&'static str] = &["webserver"];

    fn id(&self) -> &'static str {
        "consumer"
    }

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        *self.observed.borrow_mut() = Some(context.require::<WebServer>(Self::DEPENDS_ON[0])?);
        Ok(())
    }
}

#[test]
fn plugin_provides_webserver_to_dependent_plugins() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
    let mut router = block_on(EventRouter::new(lanes)).expect("create router");
    let observed = Rc::new(RefCell::new(None));
    let plugin_id = barracuda_plugin_manager::PluginId::try_from("webserver")
        .expect("valid WebServer Plugin ID");

    manager
        .register(&mut router, WebServerPlugin::new(&mut plugin_context()))
        .expect("register WebServer Plugin");
    manager
        .register(
            &mut router,
            Consumer {
                observed: Rc::clone(&observed),
            },
        )
        .expect("register consumer");
    assert!(observed.borrow().is_some());
    assert_eq!(manager.component_ids(&plugin_id).map(<[_]>::len), Some(0));
    assert!(block_on(futures_lite::future::poll_once(&mut router)).is_none());
}

#[test]
fn plugin_requires_a_system_task_spawner_during_startup() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
    let mut router = block_on(EventRouter::new(lanes)).expect("create router");

    manager
        .register(&mut router, WebServerPlugin::new(&mut plugin_context()))
        .expect("register WebServer Plugin");

    let error = manager
        .start(&mut router)
        .expect_err("missing task spawner must fail");
    assert!(matches!(error, PluginStartError::Start(_)));
    assert!(error.to_string().contains("task spawner is unavailable"));
}

#[embassy_executor::task]
async fn start_webserver_task(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
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
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let mut router = EventRouter::new(lanes)
            .await
            .map_err(|error| error.to_string())?;

        manager.install_task_spawner(spawner);
        manager
            .register(&mut router, WebServerPlugin::new(&mut plugin_context()))
            .map_err(|error| error.to_string())?;
        manager
            .start(&mut router)
            .map_err(|error| error.to_string())
    }
    .await;
    let _ignored = completed.send(result);
}

#[test]
fn plugin_starts_its_embassy_server_task() {
    let (completed, result) = sync_channel(1);
    std::thread::Builder::new()
        .name(String::from("webserver-plugin-test"))
        .stack_size(EXECUTOR_THREAD_STACK_SIZE)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(start_webserver_task(spawner, completed))
                    .expect("spawn WebServer Plugin test");
            });
        })
        .expect("spawn WebServer Plugin executor thread");

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("WebServer Plugin startup timed out")
        .expect("WebServer Plugin failed to start");
}
