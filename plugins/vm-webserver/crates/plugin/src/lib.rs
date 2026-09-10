//! Lua adapter for Barracuda's inbound WebServer capability.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_channel::{Receiver, Sender};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_filesystem_plugin::{VmFile, VmFileReader, VmFileTransfer};
use barracuda_vm_plugin::{
    Error, Function, Lua, LuaPackage, LuaPackageRegistry, Package, Result, Table, UserDataHandle,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{ErrorType, Read};
use futures_channel::oneshot;
use spin::Mutex;

/// HTTP prefix owned by the VM WebServer adapter.
pub const VM_WEB_PATH: &str = "/vm";
/// Maximum simultaneously active Lua WebServer mounts.
pub const MAX_ACTIVE_MOUNTS: usize = 4;
/// Number of requests that may wait behind one active Lua handler call.
pub const REQUEST_QUEUE_DEPTH: usize = 1;
/// Maximum UTF-8 byte length of a mount name.
pub const MOUNT_MAX_BYTES: usize = 64;

const HANDLER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_STREAM_CHUNK_BYTES: usize = 1024;

const INSTALL_ADAPTER: &str = r#"
local webserver = require("webserver")
local native_serve = webserver.__serve
webserver.__serve = nil

function webserver.serve(options)
    if type(options) ~= "table" then
        error("bad argument #1 to 'serve' (table expected)", 2)
    end
    if type(options.mount) ~= "string" then
        error("bad field 'mount' (string expected)", 2)
    end
    if type(options.handler) ~= "function" then
        error("bad field 'handler' (function expected)", 2)
    end
    return native_serve(options.mount, function(method, path, body)
        return options.handler({ method=method, path=path, body=body })
    end)
end

function webserver.file(file, content_type, status)
    local take = file and file.__barracuda_take_reader
    if type(take) ~= "function" then
        error("bad argument #1 to 'file' (open VM file expected)", 2)
    end
    return {
        __kind = "file",
        __file = take(file),
        status = status or 200,
        content_type = content_type or "application/octet-stream",
    }
end
"#;

/// Installs bounded inbound HTTP handling into every Lua VM.
#[barracuda_plugin::macros::plugin]
pub struct VmWebServerPlugin;

impl VmWebServerPlugin {
    /// Creates the stateless VM WebServer adapter.
    #[must_use]
    pub const fn new<Peripherals, ExposedIo>(
        _context: &mut PluginContext<Peripherals, ExposedIo>,
    ) -> Self {
        Self
    }
}

impl Default for VmWebServerPlugin {
    fn default() -> Self {
        Self
    }
}

impl Plugin for VmWebServerPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let files = *context.require::<VmFileTransfer>("vm-filesystem")?;
        let webserver = context.require::<WebServer>("webserver")?;
        let state = Arc::new(SharedState::new());
        let route = webserver
            .serve_http_prefix(VM_WEB_PATH, VmEndpoint::new(Arc::clone(&state)))
            .map_err(PluginError::registration)?;
        let package = VmWebServerPackage { state, files };
        let package = packages
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(route);
        context.retain(package);
        Ok(())
    }
}

struct VmWebServerPackage {
    state: Arc<SharedState>,
    files: VmFileTransfer,
}

impl Package for VmWebServerPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let state = Arc::clone(&self.state);
        let files = self.files;
        lua.register_lib("webserver", move |package| {
            package.register_async("__serve", move |(mount, handler): (String, Function)| {
                let state = Arc::clone(&state);
                async move { Some(serve(state, files, mount, handler).await) }
            })
        })?;
        lua.load(INSTALL_ADAPTER).exec()
    }
}

impl LuaPackage for VmWebServerPackage {
    fn name(&self) -> &'static str {
        "webserver"
    }

    fn revoke(&self) {
        self.state.revoke();
    }
}

struct SharedState {
    routes: Mutex<Vec<Route>>,
    next_id: AtomicUsize,
    active: AtomicBool,
}

impl SharedState {
    const fn new() -> Self {
        Self {
            routes: Mutex::new(Vec::new()),
            next_id: AtomicUsize::new(0),
            active: AtomicBool::new(true),
        }
    }

    fn register(self: &Arc<Self>, mount: String) -> Result<(Receiver<RequestJob>, RouteLease)> {
        validate_mount(&mount)?;
        let mut routes = self.routes.lock();
        if !self.active.load(Ordering::Acquire) {
            return Err(Error::runtime("VM WebServer package has been revoked"));
        }
        if routes.iter().any(|route| route.mount == mount) {
            return Err(Error::runtime("VM WebServer mount is already in use"));
        }
        if routes.len() >= MAX_ACTIVE_MOUNTS {
            return Err(Error::runtime("VM WebServer mount capacity is busy"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = async_channel::bounded(REQUEST_QUEUE_DEPTH);
        routes.push(Route { id, mount, sender });
        Ok((
            receiver,
            RouteLease {
                state: Arc::clone(self),
                id,
            },
        ))
    }

    fn resolve(&self, path: &str) -> Option<(Sender<RequestJob>, String)> {
        let suffix = path.strip_prefix(VM_WEB_PATH)?.strip_prefix('/')?;
        let (mount, relative) = match suffix.split_once('/') {
            Some((mount, "")) => (mount, String::from("/")),
            Some((mount, relative)) => (mount, format!("/{relative}")),
            None => (suffix, String::from("/")),
        };
        if mount.is_empty() {
            return None;
        }
        self.routes
            .lock()
            .iter()
            .find(|route| route.mount == mount)
            .map(|route| (route.sender.clone(), relative))
    }

    fn remove(&self, id: usize) {
        let mut routes = self.routes.lock();
        if let Some(index) = routes.iter().position(|route| route.id == id) {
            routes[index].sender.close();
            routes.remove(index);
        }
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        let mut routes = self.routes.lock();
        for route in routes.drain(..) {
            route.sender.close();
        }
    }
}

struct Route {
    id: usize,
    mount: String,
    sender: Sender<RequestJob>,
}

struct RouteLease {
    state: Arc<SharedState>,
    id: usize,
}

impl Drop for RouteLease {
    fn drop(&mut self) {
        self.state.remove(self.id);
    }
}

struct RequestJob {
    method: &'static str,
    path: String,
    body: Vec<u8>,
    response: oneshot::Sender<HandlerResponse>,
}

enum HandlerResponse {
    Buffered {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    },
    File {
        status: u16,
        content_type: &'static str,
        length: usize,
        reader: VmFileProxyReader,
    },
}

impl HandlerResponse {
    fn error(status: u16, message: &'static [u8]) -> Self {
        Self::Buffered {
            status,
            content_type: "text/plain; charset=utf-8",
            body: message.to_vec(),
        }
    }

    fn into_http(self) -> HttpResponse {
        match self {
            Self::Buffered {
                status,
                content_type,
                body,
            } => HttpResponse::new(status, content_type, body),
            Self::File {
                status,
                content_type,
                length,
                reader,
            } => HttpResponse::stream(status, content_type, length, reader),
        }
    }
}

struct VmEndpoint {
    state: Arc<SharedState>,
    response_timeout: Duration,
}

impl VmEndpoint {
    fn new(state: Arc<SharedState>) -> Self {
        Self {
            state,
            response_timeout: HANDLER_RESPONSE_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(state: Arc<SharedState>, response_timeout: Duration) -> Self {
        Self {
            state,
            response_timeout,
        }
    }

    async fn dispatch(&self, request: HttpRequest) -> HandlerResponse {
        let Some((requests, path)) = self.state.resolve(request.path()) else {
            return HandlerResponse::error(404, b"VM WebServer mount not found");
        };
        let method = method_name(request.method());
        let body = request.into_body();
        let (response_sender, receive) = oneshot::channel();
        let request = RequestJob {
            method,
            path,
            body,
            response: response_sender,
        };
        match requests.try_send(request) {
            Ok(()) => match with_timeout(self.response_timeout, receive).await {
                Ok(Ok(response)) => response,
                Ok(Err(_canceled)) => HandlerResponse::error(503, b"VM WebServer handler stopped"),
                Err(_timeout) => HandlerResponse::error(504, b"VM WebServer handler timed out"),
            },
            Err(error) if error.is_full() => {
                HandlerResponse::error(503, b"VM WebServer handler is busy")
            }
            Err(_closed) => HandlerResponse::error(503, b"VM WebServer handler stopped"),
        }
    }
}

impl HttpEndpoint for VmEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move { self.dispatch(request).await.into_http() })
    }
}

async fn serve(
    state: Arc<SharedState>,
    files: VmFileTransfer,
    mount: String,
    handler: Function,
) -> Result<()> {
    let (requests, _lease) = state.register(mount)?;
    while let Ok(request) = requests.recv().await {
        let RequestJob {
            method,
            path,
            body,
            response: response_sender,
        } = request;
        let prepared = match handler.call_async::<_, Table>((method, path, body)).await {
            Ok(response) => response_from_lua(files, response)
                .await
                .unwrap_or_else(|_error| PreparedResponse::error(500, b"invalid VM response")),
            Err(_error) => PreparedResponse::error(500, b"VM handler failed"),
        };
        let PreparedResponse { response, file } = prepared;
        let _delivered = response_sender.send(response).is_ok();
        if let Some(file) = file {
            file.run().await;
        }
    }
    Ok(())
}

struct PreparedResponse {
    response: HandlerResponse,
    file: Option<FilePump>,
}

impl PreparedResponse {
    fn error(status: u16, message: &'static [u8]) -> Self {
        Self {
            response: HandlerResponse::error(status, message),
            file: None,
        }
    }
}

struct FileReadJob {
    length: usize,
    response: oneshot::Sender<core::result::Result<Vec<u8>, FileError>>,
}

type FileError = <VmFileReader as ErrorType>::Error;

struct VmFileProxyReader {
    requests: Sender<FileReadJob>,
    eof: bool,
}

impl ErrorType for VmFileProxyReader {
    type Error = FileError;
}

impl Read for VmFileProxyReader {
    async fn read(&mut self, buffer: &mut [u8]) -> core::result::Result<usize, Self::Error> {
        if self.eof || buffer.is_empty() {
            return Ok(0);
        }
        let (response, receive) = oneshot::channel();
        self.requests
            .send(FileReadJob {
                length: buffer.len().min(FILE_STREAM_CHUNK_BYTES),
                response,
            })
            .await
            .map_err(|_closed| FileError::Io)?;
        let bytes = receive.await.map_err(|_canceled| FileError::Io)??;
        if bytes.len() > buffer.len() {
            return Err(FileError::Io);
        }
        buffer[..bytes.len()].copy_from_slice(&bytes);
        self.eof = bytes.is_empty();
        Ok(bytes.len())
    }
}

struct FilePump {
    requests: Receiver<FileReadJob>,
    reader: VmFileReader,
}

impl FilePump {
    async fn run(self) {
        let Self {
            requests,
            mut reader,
        } = self;
        while let Ok(request) = requests.recv().await {
            let mut bytes = alloc::vec![0; request.length];
            let result = reader.read(&mut bytes).await.and_then(|read| {
                if read > bytes.len() {
                    return Err(FileError::Io);
                }
                bytes.truncate(read);
                Ok(bytes)
            });
            let eof = matches!(&result, Ok(bytes) if bytes.is_empty());
            if request.response.send(result).is_err() || eof {
                break;
            }
        }
        let _cleanup = reader.close().await;
    }
}

async fn response_from_lua(files: VmFileTransfer, response: Table) -> Result<PreparedResponse> {
    let status = response.get::<_, Option<i64>>("status")?.unwrap_or(200);
    let status = u16::try_from(status)
        .ok()
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| Error::runtime("invalid HTTP response status"))?;
    let content_type = response
        .get::<_, Option<String>>("content_type")?
        .unwrap_or_else(|| String::from("text/plain; charset=utf-8"));
    let content_type = parse_content_type(&content_type)?;
    match response.get::<_, Option<String>>("__kind")?.as_deref() {
        None => Ok(PreparedResponse {
            response: HandlerResponse::Buffered {
                status,
                content_type,
                body: response
                    .get::<_, Option<Vec<u8>>>("body")?
                    .unwrap_or_default(),
            },
            file: None,
        }),
        Some("file") => {
            let file = response.get::<_, UserDataHandle<VmFile>>("__file")?;
            let (length, reader) = files.take_reader(file).await?.into_parts();
            let (requests, file_requests) = async_channel::bounded(1);
            Ok(PreparedResponse {
                response: HandlerResponse::File {
                    status,
                    content_type,
                    length,
                    reader: VmFileProxyReader {
                        requests,
                        eof: false,
                    },
                },
                file: Some(FilePump {
                    requests: file_requests,
                    reader,
                }),
            })
        }
        Some(_) => Err(Error::runtime("invalid VM WebServer response kind")),
    }
}

fn validate_mount(mount: &str) -> Result<()> {
    if mount.is_empty()
        || mount.len() > MOUNT_MAX_BYTES
        || !mount.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err(Error::runtime("invalid VM WebServer mount"));
    }
    Ok(())
}

const fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Trace => "TRACE",
        HttpMethod::Patch => "PATCH",
        _ => "OTHER",
    }
}

fn parse_content_type(content_type: &str) -> Result<&'static str> {
    match content_type {
        "text/plain" => Ok("text/plain"),
        "text/plain; charset=utf-8" => Ok("text/plain; charset=utf-8"),
        "text/html" => Ok("text/html"),
        "text/html; charset=utf-8" => Ok("text/html; charset=utf-8"),
        "text/css" => Ok("text/css"),
        "application/json" => Ok("application/json"),
        "application/javascript" => Ok("application/javascript"),
        "application/octet-stream" => Ok("application/octet-stream"),
        "application/wasm" => Ok("application/wasm"),
        "image/svg+xml" => Ok("image/svg+xml"),
        "image/png" => Ok("image/png"),
        "image/jpeg" => Ok("image/jpeg"),
        "image/gif" => Ok("image/gif"),
        "image/x-icon" => Ok("image/x-icon"),
        "font/woff2" => Ok("font/woff2"),
        _ => Err(Error::runtime("unsupported HTTP response content type")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    extern crate std;

    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::{vec, vec::Vec};

    use barracuda_plugin::manager::{Plugin, PluginDeclaration, PluginRequirements};
    use barracuda_vfs::{MountOptions, Vfs};
    use barracuda_vfs_memfs::MemFs;
    use barracuda_vm_filesystem_plugin::{VmFileTransfer, package_for_test};
    use barracuda_vm_plugin::{Package, Result};
    use barracuda_vm_runtime::FixedMemoryLua;
    use barracuda_webserver_plugin::{HttpMethod, HttpRequest};
    use embassy_time::Duration;
    use embedded_io_async::Read as _;
    use futures_lite::future::{block_on, zip};

    use super::{HandlerResponse, SharedState, VmEndpoint, VmWebServerPackage, VmWebServerPlugin};

    #[test]
    fn plugin_declares_vm_filesystem_and_webserver_dependencies() {
        assert_eq!(
            VmWebServerPlugin::DEPENDS_ON,
            ["vm", "vm-filesystem", "webserver"]
        );
        assert_eq!(VmWebServerPlugin::REQUIREMENTS, PluginRequirements::new());
    }

    #[test]
    fn lua_handler_receives_relative_requests_and_returns_buffered_responses() -> Result<()> {
        block_on(async {
            let state = Arc::new(SharedState::new());
            let package = VmWebServerPackage {
                state: Arc::clone(&state),
                files: VmFileTransfer::new(),
            };
            let mut fixed = FixedMemoryLua::new(64 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            package.install(fixed.lua_mut())?;
            let execution = fixed
                .lua_mut()
                .load(
                    "local webserver=require('webserver') \
                     webserver.serve({mount='echo', handler=function(request) \
                       return {status=201, content_type='text/plain', \
                         body=request.method .. ' ' .. request.path .. ' ' .. request.body} \
                     end})",
                )
                .exec_async();
            let request = async {
                let response = VmEndpoint::new(Arc::clone(&state))
                    .dispatch(HttpRequest::with_path(
                        HttpMethod::Post,
                        String::from("/vm/echo/items"),
                        b"value".to_vec(),
                    ))
                    .await;
                state.revoke();
                match response {
                    HandlerResponse::Buffered {
                        status,
                        content_type,
                        body,
                    } => {
                        assert_eq!(status, 201);
                        assert_eq!(content_type, "text/plain");
                        assert_eq!(body, b"POST /items value");
                    }
                    HandlerResponse::File { .. } => panic!("expected buffered response"),
                }
                Result::<()>::Ok(())
            };
            let (execution, request) = zip(execution, request).await;
            request?;
            execution
        })
    }

    #[test]
    fn streams_an_existing_vm_file_and_closes_its_lua_handle() -> Result<()> {
        block_on(async {
            let vfs = Vfs::new();
            vfs.mount(
                "/data",
                MemFs::new().into_backend(),
                MountOptions::read_write(),
            )
            .await
            .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let filesystem = vfs
                .scoped_mounts([("/data", "/data")])
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            filesystem
                .write("/data/index.html", b"<h1>VM</h1>")
                .await
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;

            let state = Arc::new(SharedState::new());
            let webserver = VmWebServerPackage {
                state: Arc::clone(&state),
                files: VmFileTransfer::new(),
            };
            let mut fixed = FixedMemoryLua::new(96 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io =
                barracuda_vm_builtin_packages::BuiltinPackages::all().install(fixed.lua_mut())?;
            package_for_test(filesystem).install(fixed.lua_mut())?;
            webserver.install(fixed.lua_mut())?;
            let execution = fixed
                .lua_mut()
                .load(
                    "local webserver=require('webserver') \
                     local served_file \
                     webserver.serve({mount='site', handler=function(_) \
                       served_file=assert(io.open('/data/index.html', 'rb')) \
                       return webserver.file(served_file, 'text/html') \
                     end}) \
                     assert(io.type(served_file) == 'closed file')",
                )
                .exec_async();
            let request = async {
                let response = VmEndpoint::new(Arc::clone(&state))
                    .dispatch(HttpRequest::with_path(
                        HttpMethod::Get,
                        String::from("/vm/site/"),
                        Vec::new(),
                    ))
                    .await;
                state.revoke();
                let (status, content_type, length, mut reader) = match response {
                    HandlerResponse::File {
                        status,
                        content_type,
                        length,
                        reader,
                    } => (status, content_type, length, reader),
                    HandlerResponse::Buffered { status, body, .. } => panic!(
                        "expected file response, got {status}: {}",
                        String::from_utf8_lossy(&body)
                    ),
                };
                assert_eq!(status, 200);
                assert_eq!(content_type, "text/html");
                assert_eq!(length, 11);
                let mut bytes = vec![0; length];
                let mut offset = 0;
                while offset < bytes.len() {
                    let read = reader
                        .read(&mut bytes[offset..])
                        .await
                        .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
                    if read == 0 {
                        break;
                    }
                    offset += read;
                }
                assert_eq!(bytes, b"<h1>VM</h1>");
                Result::<()>::Ok(())
            };
            let (execution, request) = zip(execution, request).await;
            request?;
            execution
        })
    }

    #[test]
    fn unknown_and_busy_mounts_return_bounded_errors() {
        block_on(async {
            let state = Arc::new(SharedState::new());
            let endpoint = VmEndpoint::new(Arc::clone(&state));
            let missing = endpoint
                .dispatch(HttpRequest::with_path(
                    HttpMethod::Get,
                    String::from("/vm/missing"),
                    Vec::new(),
                ))
                .await;
            assert!(matches!(
                missing,
                HandlerResponse::Buffered { status: 404, .. }
            ));

            let (_requests, _lease) = state
                .register(String::from("busy"))
                .expect("register mount");
            let (response_sender, _receive) = futures_channel::oneshot::channel();
            let sender = state.resolve("/vm/busy").expect("resolve mount").0;
            sender
                .try_send(super::RequestJob {
                    method: "GET",
                    path: String::from("/"),
                    body: Vec::new(),
                    response: response_sender,
                })
                .expect("fill queue");
            let busy = endpoint
                .dispatch(HttpRequest::with_path(
                    HttpMethod::Get,
                    String::from("/vm/busy"),
                    Vec::new(),
                ))
                .await;
            assert!(matches!(
                busy,
                HandlerResponse::Buffered { status: 503, .. }
            ));

            state.revoke();
            assert!(state.register(String::from("late")).is_err());
        });
    }

    #[test]
    fn handler_timeout_releases_the_connection_worker() {
        block_on(async {
            let state = Arc::new(SharedState::new());
            let (_requests, _lease) = state
                .register(String::from("silent"))
                .expect("register silent mount");
            let endpoint = VmEndpoint::with_timeout(Arc::clone(&state), Duration::from_millis(1));
            let response = endpoint
                .dispatch(HttpRequest::with_path(
                    HttpMethod::Get,
                    String::from("/vm/silent"),
                    Vec::new(),
                ))
                .await;
            assert!(matches!(
                response,
                HandlerResponse::Buffered { status: 504, .. }
            ));
        });
    }
}
