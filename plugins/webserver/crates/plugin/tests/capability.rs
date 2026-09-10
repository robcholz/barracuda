//! WebServer capability sharing integration test.

#![allow(clippy::expect_used)]
#![recursion_limit = "256"]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin::api::{
    BoardInfo, ClientFactory, Hardware, PlatformInfo, PluginContext, TargetIdentity,
};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStartError,
};
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use embassy_executor::{Executor, Spawner};
use futures_lite::future::block_on;

const EXECUTOR_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

fn plugin_context() -> PluginContext {
    let stack = never_embassy_stack();
    let info = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    PluginContext::new(info, stack, ClientFactory::plaintext(stack))
}

struct Consumer {
    observed: Rc<RefCell<Option<Rc<WebServer>>>>,
}

impl PluginDeclaration for Consumer {
    const ID: &'static str = "consumer";
    const DEPENDS_ON: &'static [&'static str] = &["webserver"];
}

impl Plugin for Consumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        *self.observed.borrow_mut() =
            Some(context.require::<WebServer>(<Self as PluginDeclaration>::DEPENDS_ON[0])?);
        Ok(())
    }
}

#[test]
fn plugin_provides_webserver_to_dependent_plugins() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let observed = Rc::new(RefCell::new(None));

    manager
        .register(WebServerPlugin::new(&mut plugin_context()))
        .expect("register WebServer Plugin");
    manager
        .register(Consumer {
            observed: Rc::clone(&observed),
        })
        .expect("register consumer");
    assert!(observed.borrow().is_some());
}

#[test]
fn plugin_requires_a_system_task_spawner_during_startup() {
    let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    manager
        .register(WebServerPlugin::new(&mut plugin_context()))
        .expect("register WebServer Plugin");

    let error = manager.start().expect_err("missing task spawner must fail");
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
        manager.install_task_spawner(spawner);
        manager
            .register(WebServerPlugin::new(&mut plugin_context()))
            .map_err(|error| error.to_string())?;
        manager.start().map_err(|error| error.to_string())
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
                spawner.spawn(
                    start_webserver_task(spawner, completed).expect("spawn WebServer Plugin test"),
                );
            });
        })
        .expect("spawn WebServer Plugin executor thread");

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("WebServer Plugin startup timed out")
        .expect("WebServer Plugin failed to start");
}
