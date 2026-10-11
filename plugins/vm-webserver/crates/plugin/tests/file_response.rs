//! A Lua handler serves a VM file through the WebServer, with the VM,
//! VM Filesystem, WebServer and VM WebServer Plugins registered as in
//! production and the request sent as real HTTP.

#![allow(clippy::expect_used, clippy::panic, missing_docs)]
#![recursion_limit = "256"]

use std::{cell::RefCell, rc::Rc};

use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo, PluginContext, TargetIdentity};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_vm_filesystem_plugin::VmFilesystemPlugin;
use barracuda_vm_plugin::{LuaPackageRegistry, VmPlugin};
use barracuda_vm_runtime::FixedMemoryLua;
use barracuda_vm_webserver_plugin::VmWebServerPlugin;
use barracuda_webserver_plugin::{WebServer, WebServerPlugin};
use barracuda_workflow_plugin::WorkflowPlugin;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use futures_lite::future::{block_on, zip};
use picoserve::io::Socket;
use picoserve::time::{Duration, TimeoutError, Timer};

type Observed = Rc<RefCell<Option<(Rc<LuaPackageRegistry>, Rc<WebServer>)>>>;

/// Observes what a Lua state and an HTTP client see.
struct Observer(Observed);

impl PluginDeclaration for Observer {
    const ID: &'static str = "observer";
    const DEPENDS_ON: &'static [&'static str] = &["vm", "webserver"];
}

impl Plugin for Observer {
    fn register<Storage: PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()> {
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let webserver = context.require::<WebServer>("webserver")?;
        self.0.replace(Some((packages, webserver)));
        Ok(())
    }
}

#[test]
fn a_lua_handler_streams_a_vm_file_and_closes_its_handle() {
    block_on(async {
        install_global_memory_vfs().await.expect("test VFS");
        let partition = memory_partition(64 * 1024).await.expect("database region");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("Plugin storage");
        manager.install_vfs(barracuda_vfs::global_namespace().await);
        let mut context = PluginContext::new(
            TargetIdentity::new(
                PlatformInfo::new("test", "test", "test-arch", "hosted"),
                BoardInfo::new("test-board", Hardware::new("test-chip")),
            ),
            never_embassy_stack(),
        );
        manager
            .register(WorkflowPlugin::new(&mut context))
            .expect("workflow");
        manager.register(VmPlugin::new(&mut context)).expect("vm");
        manager
            .register(VmFilesystemPlugin::new(&mut context))
            .expect("vm-filesystem");
        manager
            .register(WebServerPlugin::new(&mut context))
            .expect("webserver");
        manager
            .register(VmWebServerPlugin::new(&mut context))
            .expect("vm-webserver");
        let observed = Rc::new(RefCell::new(None));
        manager
            .register(Observer(Rc::clone(&observed)))
            .expect("observer");
        let (packages, webserver) = observed.take().expect("observed capabilities");

        // A run's Lua state, configured as the VM configures every run.
        let mut fixed = FixedMemoryLua::with_default_heap().expect("Lua heap");
        let _io = barracuda_vm_builtin_packages::BuiltinPackages::new(
            barracuda_vm_builtin_packages::math::SeedSource::unavailable(),
        )
        .install(fixed.lua_mut())
        .expect("built-in packages");
        packages
            .install(fixed.lua_mut())
            .expect("registered packages");
        let execution = fixed
            .lua_mut()
            .load(
                "local page=assert(io.open('/data/index.html', 'w')) \
                 assert(page:write('<h1>VM</h1>')) assert(page:close()) \
                 local webserver=require('webserver') \
                 local served_file \
                 webserver.serve({mount='site', handler=function(_) \
                   served_file=assert(io.open('/data/index.html', 'rb')) \
                   return webserver.file(served_file, 'text/html') \
                 end}) \
                 assert(io.type(served_file) == 'closed file')",
            )
            .exec_async();
        let client = async {
            let response = get(&webserver, "/vm/site/").await;
            // Unloading revokes the package, which ends the handler's serve loop.
            manager
                .unload(&"vm-webserver".try_into().expect("Plugin ID"))
                .await
                .expect("unload VM WebServer");
            response
        };
        let (execution, response) = zip(execution, client).await;
        execution.expect("Lua handler");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response headers");
        let head = std::str::from_utf8(&response[..split]).expect("ASCII headers");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("Content-Type: text/html"), "{head}");
        assert_eq!(&response[split + 4..], b"<h1>VM</h1>");
    });
}

async fn get(server: &WebServer, path: &str) -> Vec<u8> {
    let input = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut output = Output(Vec::new());
    server
        .serve_connection(
            TestTimer,
            &mut [0; 2048],
            TestSocket {
                input: Input(input.into_bytes(), 0),
                output: &mut output,
            },
        )
        .await
        .expect("serve connection");
    output.0
}

struct Input(Vec<u8>, usize);

impl ErrorType for Input {
    type Error = ErrorKind;
}

impl Read for Input {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, ErrorKind> {
        let rest = &self.0[self.1..];
        let count = bytes.len().min(rest.len());
        bytes[..count].copy_from_slice(&rest[..count]);
        self.1 += count;
        Ok(count)
    }
}

struct Output(Vec<u8>);

impl ErrorType for Output {
    type Error = ErrorKind;
}

impl Write for Output {
    async fn write(&mut self, bytes: &[u8]) -> Result<usize, ErrorKind> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

impl picoserve::io::Write for Output {
    async fn write_with<F: FnOnce(&mut [u8]) -> (usize, R), R>(
        &mut self,
        f: F,
    ) -> Result<R, ErrorKind> {
        let mut scratch = [0; 128];
        let (count, result) = f(&mut scratch);
        self.0.extend_from_slice(&scratch[..count]);
        Ok(result)
    }
}

struct TestSocket<'a> {
    input: Input,
    output: &'a mut Output,
}

impl Socket<()> for TestSocket<'_> {
    type Error = ErrorKind;
    type ReadHalf<'a>
        = &'a mut Input
    where
        Self: 'a;
    type WriteHalf<'a>
        = &'a mut Output
    where
        Self: 'a;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        (&mut self.input, self.output)
    }

    async fn abort<T: Timer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }

    async fn shutdown<T: Timer<()>>(
        self,
        _: &picoserve::Timeouts,
        _: &mut T,
    ) -> Result<(), picoserve::Error<ErrorKind>> {
        Ok(())
    }
}

struct TestTimer;

impl Timer<()> for TestTimer {
    async fn delay(&self, _: Duration) {
        core::future::pending().await
    }

    async fn run_with_timeout<F: Future>(
        &self,
        _: Duration,
        future: F,
    ) -> Result<F::Output, TimeoutError> {
        Ok(future.await)
    }
}
