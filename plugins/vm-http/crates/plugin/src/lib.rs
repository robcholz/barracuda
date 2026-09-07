//! Lua adapter for Barracuda's bounded outbound HTTP capability.

#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    rc::Rc,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

use async_channel::{Receiver, Sender};
use barracuda_http_plugin::{Http, HttpError, HttpHeader, HttpMethod, HttpRequest, HttpResponse};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_vm_plugin::{Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result, Table};
use embassy_futures::select::{Either, Either3, select, select3};
use futures_channel::oneshot;
use futures_util::stream::{FuturesUnordered, StreamExt as _};

const REQUEST_QUEUE_DEPTH: usize = 2;

const INSTALL_ADAPTER: &str = r#"
local http = require("http")
local native_request = http.__request
http.__request = nil

function http.request(request)
    local status, body, message = native_request(request)
    if status == nil then
        if message == nil then error(body, 2) end
        return nil, message
    end
    return { status=status, body=body }
end
"#;

/// Installs the bounded `http` package into every Lua VM.
#[barracuda_plugin::macros::plugin]
pub struct VmHttpPlugin {
    runtime: Option<VmHttpRuntime>,
}

struct VmHttpRuntime {
    http: Rc<Http>,
    requests: Receiver<HttpRequestJob>,
    state: Arc<HttpPackageState>,
}

impl VmHttpPlugin {
    /// Creates an unregistered VM HTTP adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for VmHttpPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let http = context.require::<Http>("http")?;
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let (package, requests) = HttpPackage::new();
        let state = Arc::clone(&package.state);
        let registration = packages
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        self.runtime = Some(VmHttpRuntime {
            http,
            requests,
            state,
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
            .ok_or_else(|| PluginError::registration(HttpRuntimeUnavailable))?;
        let cancellation = context.task_token();
        context
            .task_spawner()?
            .spawn(vm_http_task(runtime, cancellation))
            .map_err(PluginError::registration)
    }
}

#[embassy_executor::task]
async fn vm_http_task(runtime: VmHttpRuntime, cancellation: PluginTaskToken) {
    serve(runtime, cancellation).await;
}

type RequestFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

async fn serve(runtime: VmHttpRuntime, cancellation: PluginTaskToken) {
    let VmHttpRuntime {
        http,
        requests,
        state,
    } = runtime;
    let mut active = FuturesUnordered::<RequestFuture>::new();
    loop {
        if active.is_empty() {
            match select(cancellation.cancelled(), requests.recv()).await {
                Either::First(()) | Either::Second(Err(_)) => break,
                Either::Second(Ok(request)) => {
                    active.push(Box::pin(run_request(Rc::clone(&http), request)));
                }
            }
        } else {
            match select3(cancellation.cancelled(), requests.recv(), active.next()).await {
                Either3::First(()) | Either3::Second(Err(_)) => break,
                Either3::Second(Ok(request)) => {
                    active.push(Box::pin(run_request(Rc::clone(&http), request)));
                }
                Either3::Third(_) => {}
            }
        }
    }
    state.revoke();
    requests.close();
    while let Ok(request) = requests.try_recv() {
        request.respond(Err(HttpError::Transport));
    }
}

async fn run_request(http: Rc<Http>, request: HttpRequestJob) {
    let response = http.request(request.request).await;
    let _ignored = request.response.send(response);
}

struct HttpRequestJob {
    request: HttpRequest,
    response: oneshot::Sender<core::result::Result<HttpResponse, HttpError>>,
}

impl HttpRequestJob {
    #[cfg(test)]
    fn request(&self) -> &HttpRequest {
        &self.request
    }

    fn respond(self, response: core::result::Result<HttpResponse, HttpError>) {
        let _ignored = self.response.send(response);
    }
}

struct HttpPackage {
    state: Arc<HttpPackageState>,
}

impl HttpPackage {
    fn new() -> (Self, Receiver<HttpRequestJob>) {
        let (requests, receiver) = async_channel::bounded(REQUEST_QUEUE_DEPTH);
        let state = Arc::new(HttpPackageState {
            requests,
            active: AtomicBool::new(true),
        });
        (Self { state }, receiver)
    }
}

impl Package for HttpPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let state = Arc::clone(&self.state);
        lua.register_lib("http", move |http| {
            http.register_async("__request", move |request: Table| {
                let state = Arc::clone(&state);
                let request = parse_request(request);
                async move {
                    Some(match request {
                        Ok(request) => execute_request(state, request).await,
                        Err(error) => Err(error),
                    })
                }
            })
        })?;
        lua.load(INSTALL_ADAPTER).exec()
    }
}

impl LuaPackage for HttpPackage {
    fn name(&self) -> &'static str {
        "http"
    }

    fn revoke(&self) {
        self.state.revoke();
    }
}

struct HttpPackageState {
    requests: Sender<HttpRequestJob>,
    active: AtomicBool,
}

impl HttpPackageState {
    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.requests.close();
    }
}

fn parse_request(request: Table) -> Result<HttpRequest> {
    let method = match request.get::<_, String>("method")?.as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        _ => return Err(Error::runtime("invalid HTTP method")),
    };
    let url = request.get::<_, String>("url")?;
    let body = request
        .get::<_, Option<String>>("body")?
        .unwrap_or_default();
    let headers = match request.get::<_, Option<Table>>("headers")? {
        Some(headers) => parse_headers(headers)?,
        None => Vec::new(),
    };
    Ok(HttpRequest {
        method,
        url,
        headers,
        body,
    })
}

fn parse_headers(headers: Table) -> Result<Vec<HttpHeader>> {
    let count = headers.len()?;
    let mut parsed = Vec::with_capacity(count);
    for index in 1..=count {
        let header = headers.get::<_, Table>(
            i64::try_from(index).map_err(|_| Error::runtime("too many HTTP headers"))?,
        )?;
        parsed.push(HttpHeader {
            name: header.get("name")?,
            value: header.get("value")?,
        });
    }
    Ok(parsed)
}

type LuaHttpResponse = (Option<i64>, Option<String>, Option<String>);

async fn execute_request(
    state: Arc<HttpPackageState>,
    request: HttpRequest,
) -> Result<LuaHttpResponse> {
    if !state.active.load(Ordering::Acquire) {
        return Err(Error::runtime("VM HTTP package has been revoked"));
    }
    let (response, receive) = oneshot::channel();
    state
        .requests
        .try_send(HttpRequestJob { request, response })
        .map_err(|error| {
            if error.is_closed() {
                Error::runtime("VM HTTP runtime is not available")
            } else {
                Error::runtime("VM HTTP request capacity is busy")
            }
        })?;
    match receive.await {
        Ok(Ok(response)) => Ok((Some(i64::from(response.status)), Some(response.body), None)),
        Ok(Err(error)) => Ok((None, None, Some(error.code().to_string()))),
        Err(_) => Err(Error::runtime("VM HTTP runtime is not available")),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("VM HTTP runtime was not prepared during Plugin registration")]
struct HttpRuntimeUnavailable;

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::{HttpPackage, VmHttpPlugin};
    use barracuda_http_plugin::{HttpError, HttpMethod, HttpResponse};
    use barracuda_plugin::manager::PluginDeclaration;
    use barracuda_vfs::{MountOptions, Vfs};
    use barracuda_vfs_memfs::MemFs;

    #[test]
    fn plugin_declares_http_and_vm_dependencies() {
        assert_eq!(VmHttpPlugin::DEPENDS_ON, ["http", "vm"]);
    }
    use barracuda_vm_plugin::{LuaPackage, Package, Result};
    use barracuda_vm_runtime::FixedMemoryLua;

    #[test]
    fn exposes_bounded_http_requests_to_lua() -> Result<()> {
        futures_lite::future::block_on(async {
            let (package, requests) = HttpPackage::new();
            let mut fixed = FixedMemoryLua::new(64 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io =
                barracuda_vm_builtin_packages::BuiltinPackages::all().install(fixed.lua_mut())?;
            package.install(fixed.lua_mut())?;

            let execution = fixed
                .lua_mut()
                .load(
                    "local http = require('http') \
                 local response = assert(http.request({ \
                     method='POST', url='https://example.com/items', \
                     headers={{name='content-type', value='text/plain'}}, body='hello' \
                 })) \
                 assert(response.status == 201 and response.body == 'created') \
                 local failed, code = http.request({method='GET', url='https://example.com/fail'}) \
                 assert(failed == nil and code == 'transport') \
                 assert(not pcall(http.request, {method='CONNECT', url='https://example.com'}))",
                )
                .exec_async();
            let service = async {
                let request = requests.recv().await.map_err(|_| {
                    barracuda_vm_plugin::Error::runtime("missing first HTTP request")
                })?;
                assert_eq!(request.request().method, HttpMethod::Post);
                assert_eq!(request.request().url, "https://example.com/items");
                assert_eq!(request.request().body, "hello");
                request.respond(Ok(HttpResponse {
                    status: 201,
                    body: String::from("created"),
                }));

                let request = requests.recv().await.map_err(|_| {
                    barracuda_vm_plugin::Error::runtime("missing second HTTP request")
                })?;
                request.respond(Err(HttpError::Transport));
                Result::<()>::Ok(())
            };
            let (execution, service) = futures_lite::future::zip(execution, service).await;
            service?;
            execution
        })
    }

    #[test]
    fn revoked_http_package_rejects_existing_callbacks() -> Result<()> {
        futures_lite::future::block_on(async {
            let (package, _requests) = HttpPackage::new();
            let mut fixed = FixedMemoryLua::new(64 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            package.install(fixed.lua_mut())?;
            package.revoke();
            fixed
                .lua_mut()
                .load(
                    "local http = require('http') \
                 local ok = pcall(http.request, {method='GET', url='https://example.com'}) \
                 assert(not ok)",
                )
                .exec_async()
                .await
        })
    }

    #[test]
    fn complete_standard_capability_set_fits_one_bounded_vm_heap() -> Result<()> {
        futures_lite::future::block_on(async {
            let mut vfs = Vfs::new();
            vfs.mount(
                "/data",
                MemFs::new().into_backend(),
                MountOptions::read_write(),
            )
            .await
            .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            vfs.mount(
                "/cache",
                MemFs::new().into_backend(),
                MountOptions::read_write(),
            )
            .await
            .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let filesystem = vfs
                .scoped_mounts([("/data", "/data"), ("/cache", "/cache")])
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;

            let (http, _requests) = HttpPackage::new();
            let mut fixed = FixedMemoryLua::new(96 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io =
                barracuda_vm_builtin_packages::BuiltinPackages::all().install(fixed.lua_mut())?;
            barracuda_vm_filesystem_plugin::package_for_test(filesystem)
                .install(fixed.lua_mut())?;
            barracuda_vm_time_plugin::package_for_test().install(fixed.lua_mut())?;
            http.install(fixed.lua_mut())?;
            fixed
                .lua_mut()
                .load(
                    "assert(type(require('http').request) == 'function') \
                     assert(os.date('%F %T', 0) == '1970-01-01 00:00:00') \
                     local file = assert(io.open('/data/value.txt', 'w+')) \
                     assert(file:write('value')) \
                     assert(file:seek('set', 0) == 0) \
                     assert(file:read('*a') == 'value') \
                     assert(file:close())",
                )
                .exec_async()
                .await
        })
    }
}
