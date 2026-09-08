//! Plugin-scoped VFS adapter for Lua's standard file APIs.

#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_vfs::{File, FsError, OpenOptions, ScopedVfs, SeekFrom};
use barracuda_vm_plugin::{
    Context, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result, UserData, UserDataHandle,
    UserDataMethods,
};
use embedded_io_async::{Read, Seek, Write};

const MAX_OPEN_FILES: usize = 16;
const MAX_IO_BYTES: usize = 32 * 1024;
const READ_BUFFER_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 1024;

const INSTALL_ADAPTER: &str = r##"
local io = io
local os = require("os")
local native_open = io.__file_open
local native_tmpfile = io.__file_tmpfile
io.__file_open = nil
io.__file_tmpfile = nil

local base_type = io.type
local base_input = io.input
local base_output = io.output
local wrappers = setmetatable({}, { __mode = "k" })
local methods = {}
local metatable = {
    __index = methods,
    __metatable = false,
    __tostring = function(self)
        local raw = wrappers[self]
        return raw and raw:description() or "file (closed)"
    end,
    __close = function(self)
        local raw = wrappers[self]
        if raw then raw:discard() end
    end,
}

local function raw_file(file, level)
    local raw = wrappers[file]
    if raw == nil then
        error("bad argument (FILE expected)", level or 3)
    end
    return raw
end

local function wrap(raw)
    local file = setmetatable({}, metatable)
    wrappers[file] = raw
    return file
end

local function read_one(raw, format)
    local kind = type(format)
    if kind == "number" then
        return raw:read_count(format)
    end
    if kind ~= "string" then
        error("bad argument to 'read' (invalid format)", 4)
    end
    if format == "l" or format == "*l" then
        return raw:read_line(false)
    end
    if format == "L" or format == "*L" then
        return raw:read_line(true)
    end
    if format == "a" or format == "*a" then
        return raw:read_all()
    end
    if format == "n" or format == "*n" then
        local token, message = raw:read_number()
        if token == nil then return nil, message end
        return tonumber(token)
    end
    error("bad argument to 'read' (invalid format)", 4)
end

function methods:close()
    return raw_file(self):close()
end

function methods:flush()
    return raw_file(self):flush()
end

function methods:read(...)
    local formats = table.pack(...)
    if formats.n == 0 then
        formats[1] = "l"
        formats.n = 1
    end
    local values = {}
    local count = 0
    for index = 1, formats.n do
        local value, message = read_one(raw_file(self), formats[index])
        if value == nil then
            if message ~= nil then return nil, message end
            if count == 0 then return nil end
            break
        end
        count = count + 1
        values[count] = value
    end
    return table.unpack(values, 1, count)
end

function methods:write(...)
    local values = table.pack(...)
    for index = 1, values.n do
        local kind = type(values[index])
        if kind ~= "string" and kind ~= "number" then
            error("bad argument #" .. index .. " to 'write' (string expected)", 2)
        end
        local written, message = raw_file(self):write(tostring(values[index]))
        if not written then return nil, message end
    end
    return self
end

function methods:seek(whence, offset)
    return raw_file(self):seek(whence or "cur", offset or 0)
end

function methods:setvbuf(mode, size)
    if mode ~= "no" and mode ~= "full" and mode ~= "line" then
        error("bad argument #1 to 'setvbuf' (invalid option)", 2)
    end
    if size ~= nil and (type(size) ~= "number" or size < 0 or size % 1 ~= 0) then
        error("bad argument #2 to 'setvbuf' (invalid buffer size)", 2)
    end
    return true
end

function methods:lines(...)
    local formats = table.pack(...)
    if formats.n == 0 then
        formats[1] = "l"
        formats.n = 1
    end
    return function()
        return self:read(table.unpack(formats, 1, formats.n))
    end
end

function io.open(filename, mode)
    local raw, message = native_open(filename, mode)
    if raw == nil then return nil, message end
    return wrap(raw)
end

function io.tmpfile()
    local raw, message = native_tmpfile()
    if raw == nil then return nil, message end
    return wrap(raw)
end

local current_input = base_input()
local current_output = base_output()

function io.input(file)
    if file == nil then return current_input end
    if type(file) == "string" then
        local opened, message = io.open(file, "r")
        if opened == nil then error(message, 2) end
        file = opened
    elseif base_type(file) ~= "file" and wrappers[file] == nil then
        error("bad argument #1 to 'input' (FILE expected)", 2)
    end
    current_input = file
    return file
end

function io.output(file)
    if file == nil then return current_output end
    if type(file) == "string" then
        local opened, message = io.open(file, "w")
        if opened == nil then error(message, 2) end
        file = opened
    elseif base_type(file) ~= "file" and wrappers[file] == nil then
        error("bad argument #1 to 'output' (FILE expected)", 2)
    end
    current_output = file
    return file
end

function io.read(...)
    return current_input:read(...)
end

function io.write(...)
    return current_output:write(...)
end

function io.flush()
    return current_output:flush()
end

function io.close(file)
    return (file or current_output):close()
end

function io.lines(filename, ...)
    if filename == nil then
        return current_input:lines(...)
    end
    local file, message = io.open(filename, "r")
    if file == nil then error(message, 2) end
    local formats = table.pack(...)
    if formats.n == 0 then
        formats[1] = "l"
        formats.n = 1
    end
    local iterator = function()
        local values = table.pack(file:read(table.unpack(formats, 1, formats.n)))
        if values[1] == nil then
            file:close()
            if values[2] ~= nil then error(values[2], 2) end
            return nil
        end
        return table.unpack(values, 1, values.n)
    end
    return iterator, nil, nil, file
end

function io.type(value)
    local raw = wrappers[value]
    if raw ~= nil then
        return raw:is_closed() and "closed file" or "file"
    end
    return base_type(value)
end

_G.os = os
"##;

/// Registers Lua file APIs backed only by this Plugin's scoped VFS.
#[barracuda_plugin::macros::plugin]
pub struct VmFilesystemPlugin;

impl VmFilesystemPlugin {
    /// Creates the stateless adapter Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Default for VmFilesystemPlugin {
    fn default() -> Self {
        Self
    }
}

impl Plugin for VmFilesystemPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>("vm")?;
        let package = FilePackage::new(context.filesystem()?.clone());
        let registration = registry
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct FilePackage {
    state: Arc<FilePackageState>,
}

impl FilePackage {
    fn new(filesystem: ScopedVfs) -> Self {
        Self {
            state: Arc::new(FilePackageState {
                filesystem,
                active: AtomicBool::new(true),
                open_files: AtomicUsize::new(0),
                temporary_sequence: AtomicUsize::new(0),
            }),
        }
    }
}

/// Creates the scoped package for cross-Plugin memory tests.
#[cfg(feature = "test-fixture")]
pub fn package_for_test(filesystem: ScopedVfs) -> impl LuaPackage {
    FilePackage::new(filesystem)
}

impl Package for FilePackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let open_state = Arc::clone(&self.state);
        let temporary_state = Arc::clone(&self.state);
        lua.extend_loaded_lib("io", move |io| {
            io.register_async_with(
                "__file_open",
                move |lua, (path, mode): (String, Option<String>)| {
                    let prepared = prepare_file(lua, Arc::clone(&open_state), path, mode);
                    async move { open_prepared(prepared).await }
                },
            )?;
            io.register_async_with("__file_tmpfile", move |lua, (): ()| {
                let prepared = prepare_temporary(lua, Arc::clone(&temporary_state));
                async move { open_prepared(prepared).await }
            })
        })?;
        let remove_state = Arc::clone(&self.state);
        let rename_state = Arc::clone(&self.state);
        let name_state = Arc::clone(&self.state);
        lua.extend_loaded_lib("os", move |os| {
            os.register_async("remove", move |path: String| {
                let state = Arc::clone(&remove_state);
                async move { Some(remove_path(state, path).await) }
            })?;
            os.register_async("rename", move |(from, to): (String, String)| {
                let state = Arc::clone(&rename_state);
                async move { Some(rename_path(state, from, to).await) }
            })?;
            os.register_async("tmpname", move |(): ()| {
                let state = Arc::clone(&name_state);
                async move { Some(temporary_name(state).await) }
            })
        })?;
        lua.load(INSTALL_ADAPTER).exec()
    }
}

impl LuaPackage for FilePackage {
    fn name(&self) -> &'static str {
        "vm-filesystem"
    }

    fn revoke(&self) {
        self.state.active.store(false, Ordering::Release);
    }
}

struct FilePackageState {
    filesystem: ScopedVfs,
    active: AtomicBool,
    open_files: AtomicUsize,
    temporary_sequence: AtomicUsize,
}

struct PreparedFile {
    handle: UserDataHandle<LuaFile>,
    state: Arc<FilePackageState>,
    path: String,
    options: OpenOptions,
    readable: bool,
    writable: bool,
}

fn prepare_file(
    lua: &mut Context<'_>,
    state: Arc<FilePackageState>,
    path: String,
    mode: Option<String>,
) -> Result<PreparedFile> {
    let path = resolve_path(path)?;
    let (options, readable, writable) = open_mode(mode.as_deref().unwrap_or("r"))?;
    prepare(lua, state, path, options, readable, writable, false)
}

fn prepare_temporary(lua: &mut Context<'_>, state: Arc<FilePackageState>) -> Result<PreparedFile> {
    let sequence = state.temporary_sequence.fetch_add(1, Ordering::Relaxed);
    let path = format!("/cache/lua-tmp-{sequence}");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    prepare(lua, state, path, options, true, true, true)
}

fn prepare(
    lua: &mut Context<'_>,
    state: Arc<FilePackageState>,
    path: String,
    options: OpenOptions,
    readable: bool,
    writable: bool,
    temporary: bool,
) -> Result<PreparedFile> {
    ensure_active(&state)?;
    state
        .open_files
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
            (open < MAX_OPEN_FILES).then_some(open.saturating_add(1))
        })
        .map_err(|_open| Error::runtime("Lua open file limit reached"))?;
    let handle = lua.create_userdata(LuaFile {
        package: Arc::clone(&state),
        file: None,
        counted: true,
        temporary_path: temporary.then(|| path.clone()),
    })?;
    Ok(PreparedFile {
        handle,
        state,
        path,
        options,
        readable,
        writable,
    })
}

async fn open_prepared(prepared: Result<PreparedFile>) -> Option<Result<UserDataHandle<LuaFile>>> {
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Some(Err(error)),
    };
    if let Err(error) = ensure_active(&prepared.state) {
        let _ignored = prepared.handle.with_mut(LuaFile::release);
        return Some(Err(error));
    }
    let file = match prepared
        .state
        .filesystem
        .open_with(&prepared.path, &prepared.options)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            let _ignored = prepared.handle.with_mut(LuaFile::release);
            return Some(Err(file_error(error)));
        }
    };
    let attached = prepared.handle.with_mut(|handle| {
        handle.file = Some(FileState {
            file,
            readable: prepared.readable,
            writable: prepared.writable,
            pending: VecDeque::new(),
            eof: false,
        });
    });
    match attached {
        Ok(()) => Some(Ok(prepared.handle)),
        Err(error) => Some(Err(error)),
    }
}

struct LuaFile {
    package: Arc<FilePackageState>,
    file: Option<FileState>,
    counted: bool,
    temporary_path: Option<String>,
}

impl LuaFile {
    fn release(&mut self) {
        self.file.take();
        if self.counted {
            self.package.open_files.fetch_sub(1, Ordering::AcqRel);
            self.counted = false;
        }
    }
}

impl Drop for LuaFile {
    fn drop(&mut self) {
        self.release();
    }
}

impl UserData for LuaFile {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_async_method("read_line", |file, include_newline: bool| async move {
            Some(with_file(&file, |state| Box::pin(read_line(state, include_newline))).await)
        });
        methods.add_async_method("read_count", |file, count: i64| async move {
            Some(with_file(&file, |state| Box::pin(read_count(state, count))).await)
        });
        methods.add_async_method("read_all", |file, (): ()| async move {
            Some(with_file(&file, |state| Box::pin(read_all(state))).await)
        });
        methods.add_async_method("read_number", |file, (): ()| async move {
            Some(with_file(&file, |state| Box::pin(read_number(state))).await)
        });
        methods.add_async_method("write", |file, bytes: Vec<u8>| async move {
            Some(with_file(&file, |state| Box::pin(write_bytes(state, bytes))).await)
        });
        methods.add_async_method("seek", |file, (whence, offset): (String, i64)| async move {
            Some(with_file(&file, |state| Box::pin(seek_file(state, whence, offset))).await)
        });
        methods.add_async_method("flush", |file, (): ()| async move {
            Some(with_file(&file, |state| Box::pin(flush_file(state))).await)
        });
        methods.add_async_method("close", |file, (): ()| async move {
            Some(close_file(&file).await)
        });
        methods.add_method_mut("discard", |file, (): ()| {
            file.release();
            None::<Result<()>>
        });
        methods.add_method("is_closed", |file, (): ()| Some(Ok(file.file.is_none())));
        methods.add_method("description", |file, (): ()| {
            Some(Ok(if file.file.is_some() {
                String::from("file")
            } else {
                String::from("file (closed)")
            }))
        });
    }
}

struct FileState {
    file: File,
    readable: bool,
    writable: bool,
    pending: VecDeque<u8>,
    eof: bool,
}

type FileOperation<'a, T> = Pin<Box<dyn core::future::Future<Output = Result<T>> + 'a>>;

async fn with_file<T, F>(file: &UserDataHandle<LuaFile>, operation: F) -> Result<T>
where
    F: for<'a> FnOnce(&'a mut FileState) -> FileOperation<'a, T>,
{
    let mut state = file.with_mut(|file| {
        ensure_active(&file.package)?;
        file.file
            .take()
            .ok_or_else(|| Error::runtime("attempt to use a closed file"))
    })??;
    let result = operation(&mut state).await;
    file.with_mut(|file| {
        if file.file.is_none() {
            file.file = Some(state);
        }
    })?;
    result
}

async fn close_file(file: &UserDataHandle<LuaFile>) -> Result<bool> {
    let (state, package, temporary) = file.with_mut(|file| {
        ensure_active(&file.package)?;
        let state = file
            .file
            .take()
            .ok_or_else(|| Error::runtime("attempt to use a closed file"))?;
        Ok::<_, Error>((state, Arc::clone(&file.package), file.temporary_path.take()))
    })??;
    let mut state = state;
    let flush = if state.writable {
        state.file.flush().await.map_err(file_error)
    } else {
        Ok(())
    };
    drop(state);
    let cleanup = if let Some(path) = temporary {
        package
            .filesystem
            .remove_file(&path)
            .await
            .map_err(file_error)
    } else {
        Ok(())
    };
    file.with_mut(LuaFile::release)?;
    flush?;
    cleanup?;
    Ok(true)
}

async fn fill_buffer(state: &mut FileState) -> Result<bool> {
    if state.eof {
        return Ok(false);
    }
    if !state.readable {
        return Err(Error::runtime("file is not open for reading"));
    }
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let read = state.file.read(&mut buffer).await.map_err(file_error)?;
    if read == 0 {
        state.eof = true;
        return Ok(false);
    }
    state.pending.extend(buffer.into_iter().take(read));
    Ok(true)
}

async fn peek_byte(state: &mut FileState) -> Result<Option<u8>> {
    while state.pending.is_empty() && fill_buffer(state).await? {}
    Ok(state.pending.front().copied())
}

async fn read_line(state: &mut FileState, include_newline: bool) -> Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let Some(byte) = peek_byte(state).await? else {
            return Ok((!output.is_empty()).then_some(output));
        };
        state.pending.pop_front();
        if byte == b'\n' {
            if include_newline {
                output.push(byte);
            }
            return Ok(Some(output));
        }
        ensure_read_capacity(output.len())?;
        output.push(byte);
    }
}

async fn read_count(state: &mut FileState, count: i64) -> Result<Option<Vec<u8>>> {
    let count = usize::try_from(count)
        .map_err(|_error| Error::runtime("read count must be a non-negative integer"))?;
    if count > MAX_IO_BYTES {
        return Err(Error::runtime("read count exceeds Lua file IO limit"));
    }
    if count == 0 {
        return Ok(peek_byte(state).await?.map(|_byte| Vec::new()));
    }
    let mut output = Vec::with_capacity(count);
    while output.len() < count {
        let Some(byte) = peek_byte(state).await? else {
            break;
        };
        state.pending.pop_front();
        output.push(byte);
    }
    Ok((!output.is_empty()).then_some(output))
}

async fn read_all(state: &mut FileState) -> Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        while let Some(byte) = state.pending.pop_front() {
            ensure_read_capacity(output.len())?;
            output.push(byte);
        }
        if !fill_buffer(state).await? {
            return Ok(Some(output));
        }
    }
}

async fn read_number(state: &mut FileState) -> Result<Option<Vec<u8>>> {
    while peek_byte(state)
        .await?
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        state.pending.pop_front();
    }
    if peek_byte(state).await?.is_none() {
        return Ok(None);
    }
    let mut token = Vec::new();
    while let Some(byte) = peek_byte(state).await? {
        if byte.is_ascii_whitespace() {
            break;
        }
        if token.len() >= 256 {
            return Err(Error::runtime("numeric token exceeds Lua file IO limit"));
        }
        state.pending.pop_front();
        token.push(byte);
    }
    Ok(Some(token))
}

async fn write_bytes(state: &mut FileState, bytes: Vec<u8>) -> Result<bool> {
    if !state.writable {
        return Err(Error::runtime("file is not open for writing"));
    }
    if bytes.len() > MAX_IO_BYTES {
        return Err(Error::runtime("write exceeds Lua file IO limit"));
    }
    state.file.write_all(&bytes).await.map_err(file_error)?;
    state.file.flush().await.map_err(file_error)?;
    state.eof = false;
    Ok(true)
}

async fn seek_file(state: &mut FileState, whence: String, offset: i64) -> Result<i64> {
    let pending = i64::try_from(state.pending.len())
        .map_err(|_error| Error::runtime("file buffer position is out of range"))?;
    let position = match whence.as_str() {
        "set" => SeekFrom::Start(
            u64::try_from(offset).map_err(|_error| Error::runtime("invalid seek offset"))?,
        ),
        "cur" => SeekFrom::Current(
            offset
                .checked_sub(pending)
                .ok_or_else(|| Error::runtime("invalid seek offset"))?,
        ),
        "end" => SeekFrom::End(offset),
        _ => return Err(Error::runtime("invalid seek mode")),
    };
    state.pending.clear();
    state.eof = false;
    let position = state.file.seek(position).await.map_err(file_error)?;
    i64::try_from(position).map_err(|_error| Error::runtime("file position is out of range"))
}

async fn flush_file(state: &mut FileState) -> Result<bool> {
    state.file.flush().await.map_err(file_error)?;
    Ok(true)
}

fn ensure_read_capacity(length: usize) -> Result<()> {
    if length >= MAX_IO_BYTES {
        Err(Error::runtime("read exceeds Lua file IO limit"))
    } else {
        Ok(())
    }
}

fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        Err(Error::runtime("invalid Lua file path"))
    } else {
        Ok(())
    }
}

fn resolve_path(path: String) -> Result<String> {
    validate_path(&path)?;
    Ok(path)
}

async fn remove_path(state: Arc<FilePackageState>, path: String) -> Result<bool> {
    ensure_active(&state)?;
    let path = resolve_path(path)?;
    let metadata = state.filesystem.metadata(&path).await.map_err(file_error)?;
    if metadata.is_dir() {
        state
            .filesystem
            .remove_dir(&path)
            .await
            .map_err(file_error)?;
    } else {
        state
            .filesystem
            .remove_file(&path)
            .await
            .map_err(file_error)?;
    }
    Ok(true)
}

async fn rename_path(state: Arc<FilePackageState>, from: String, to: String) -> Result<bool> {
    ensure_active(&state)?;
    let from = resolve_path(from)?;
    let to = resolve_path(to)?;
    state
        .filesystem
        .rename(&from, &to)
        .await
        .map_err(file_error)?;
    Ok(true)
}

async fn temporary_name(state: Arc<FilePackageState>) -> Result<String> {
    ensure_active(&state)?;
    for _attempt in 0..MAX_OPEN_FILES {
        let sequence = state.temporary_sequence.fetch_add(1, Ordering::Relaxed);
        let path = format!("/cache/lua-name-{sequence}");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match state.filesystem.open_with(&path, &options).await {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(FsError::AlreadyExists) => {}
            Err(error) => return Err(file_error(error)),
        }
    }
    Err(Error::runtime("unable to allocate a Lua temporary name"))
}

fn ensure_active(state: &FilePackageState) -> Result<()> {
    if state.active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("vm-filesystem package is unavailable"))
    }
}

fn open_mode(mode: &str) -> Result<(OpenOptions, bool, bool)> {
    let normalized: String = mode.chars().filter(|character| *character != 'b').collect();
    let mut options = OpenOptions::new();
    let (readable, writable) = match normalized.as_str() {
        "r" => {
            options.read(true);
            (true, false)
        }
        "w" => {
            options.write(true).truncate(true).create(true);
            (false, true)
        }
        "a" => {
            options.write(true).append(true).create(true);
            (false, true)
        }
        "r+" => {
            options.read(true).write(true);
            (true, true)
        }
        "w+" => {
            options.read(true).write(true).truncate(true).create(true);
            (true, true)
        }
        "a+" => {
            options.read(true).write(true).append(true).create(true);
            (true, true)
        }
        _ => return Err(Error::runtime("invalid Lua file open mode")),
    };
    Ok((options, readable, writable))
}

fn file_error(error: FsError) -> Error {
    Error::runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;

    use barracuda_plugin::manager::{Plugin, PluginDeclaration, PluginFilesystem};
    use barracuda_vfs::{MountOptions, Vfs};
    use barracuda_vfs_memfs::MemFs;
    use barracuda_vm_plugin::{Lua, LuaPackage, Package, Result};
    use futures_lite::future::block_on;

    use super::{FilePackage, VmFilesystemPlugin};

    async fn filesystem()
    -> core::result::Result<barracuda_vfs::ScopedVfs, Box<dyn core::error::Error>> {
        let mut vfs = Vfs::new();
        vfs.mount(
            "/data",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await?;
        vfs.mount(
            "/cache",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await?;
        let resources = MemFs::new();
        resources.write_file("/fixture.txt", b"bundled")?;
        vfs.mount(
            "/resources",
            resources.into_backend(),
            MountOptions::read_only(),
        )
        .await?;
        let workspace_resources = MemFs::new();
        workspace_resources.write_file("/common.txt", b"shared")?;
        vfs.mount(
            "/workspace/resources",
            workspace_resources.into_backend(),
            MountOptions::read_only(),
        )
        .await?;
        vfs.mount(
            "/workspace/cache",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await?;
        vfs.mount(
            "/workspace/media",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await?;
        Ok(vfs.scoped_mounts([
            ("/data", "/data"),
            ("/cache", "/cache"),
            ("/resources", "/resources"),
            ("/workspace/resources", "/workspace/resources"),
            ("/workspace/cache", "/workspace/cache"),
            ("/workspace/media", "/workspace/media"),
        ])?)
    }

    fn lua_with_file_package(
        filesystem: barracuda_vfs::ScopedVfs,
    ) -> Result<barracuda_lua_test_fixture::Installed> {
        barracuda_lua_test_fixture::install(FilePackage::new(filesystem))
    }

    #[test]
    fn plugin_declares_private_filesystem_and_vm_dependency() {
        assert_eq!(
            VmFilesystemPlugin::REQUIREMENTS.filesystem(),
            PluginFilesystem::Private
        );
        assert_eq!(VmFilesystemPlugin::DEPENDS_ON, ["vm"]);
    }

    #[test]
    fn lua_files_support_standard_open_write_seek_read_and_close() {
        block_on(async {
            let filesystem = filesystem().await?;
            let mut installed = lua_with_file_package(filesystem.clone())?;
            installed
                .run(
                    "local file = assert(io.open('/data/value.txt', 'w+')) \
                     assert(file:write('answer=', 42)) \
                     assert(file:seek('set', 0) == 0) \
                     assert(file:read('*a') == 'answer=42') \
                     assert(file:close()) \
                     assert(io.type(file) == 'closed file')",
                )
                .await?;
            assert_eq!(filesystem.read("/data/value.txt").await?, b"answer=42");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("standard Lua file lifecycle");
    }

    #[test]
    fn lua_file_lines_append_and_default_stream_redirection_work() {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem
                .write("/data/input.txt", b"first\nsecond\n")
                .await?;
            filesystem.write("/data/output.txt", b"existing\n").await?;
            let mut installed = lua_with_file_package(filesystem.clone())?;
            installed
                .run(
                    "io.input('/data/input.txt') \
                     local output = assert(io.open('/data/output.txt', 'a')) \
                     io.output(output) \
                     for line in io.lines() do io.write(line, '\\n') end \
                     assert(io.close())",
                )
                .await?;
            assert_eq!(
                filesystem.read("/data/output.txt").await?,
                b"existing\nfirst\nsecond\n"
            );
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("standard default file streams");
    }

    #[test]
    fn io_lines_closes_its_file_when_generic_for_exits_early() {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem
                .write("/data/lines.txt", b"first\nsecond\n")
                .await?;
            let mut installed = lua_with_file_package(filesystem)?;
            installed
                .run(
                    "local iterator, state, control, closing = io.lines('/data/lines.txt') \
                     assert(type(iterator) == 'function' and state == nil and control == nil) \
                     assert(io.type(closing) == 'file') \
                     assert(closing:close()) \
                     for iteration = 1, 17 do \
                         for line in io.lines('/data/lines.txt') do \
                             assert(line == 'first') \
                             break \
                         end \
                     end \
                     local final = assert(io.open('/data/final.txt', 'w')) \
                     assert(final:close())",
                )
                .await?;
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("io.lines early close");
    }

    #[test]
    fn lua_file_read_supports_multiple_standard_formats() {
        block_on(async {
            let filesystem = filesystem().await?;
            filesystem
                .write("/data/formats.txt", b"123 45\nrest")
                .await?;
            let mut installed = lua_with_file_package(filesystem)?;
            installed
                .run(
                    "local file = assert(io.open('/data/formats.txt')) \
                     local first, second, line, rest = file:read('*n', 'n', '*l', 4) \
                     assert(first == 123 and second == 45) \
                     assert(line == '' and rest == 'rest') \
                     assert(file:read(0) == nil) \
                     assert(file:close())",
                )
                .await?;
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("standard file read formats");
    }

    #[test]
    fn lua_file_api_preserves_scoped_vfs_failures() {
        block_on(async {
            let filesystem = filesystem().await?;
            let mut installed = lua_with_file_package(filesystem)?;
            installed
                .run(
                    "local file, message = io.open('/../../outside', 'r') \
                     assert(file == nil and type(message) == 'string') \
                     local relative, relative_message = io.open('relative.txt', 'w') \
                     assert(relative == nil and type(relative_message) == 'string') \
                     local missing, missing_message = io.open('/data/missing', 'r') \
                     assert(missing == nil and type(missing_message) == 'string')",
                )
                .await?;
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("scoped VFS errors");
    }

    #[test]
    fn lua_file_api_preserves_read_only_mounts_and_open_limits() {
        block_on(async {
            let filesystem = filesystem().await?;
            let mut installed = lua_with_file_package(filesystem.clone())?;
            installed
                .run(
                    "local resource = assert(io.open('/resources/fixture.txt', 'r')) \
                     assert(resource:read('*a') == 'bundled') \
                     assert(resource:close()) \
                     local shared = assert(io.open('/workspace/resources/common.txt', 'r')) \
                     assert(shared:read('*a') == 'shared') \
                     assert(shared:close()) \
                     local denied, message = io.open('/resources/new.txt', 'w') \
                     assert(denied == nil and type(message) == 'string') \
                     local shared_denied, shared_message = \
                         io.open('/workspace/resources/new.txt', 'w') \
                     assert(shared_denied == nil and type(shared_message) == 'string') \
                     local media = assert(io.open('/workspace/media/result.txt', 'w')) \
                     assert(media:write('result') and media:close()) \
                     local files = {} \
                     for index = 1, 16 do \
                         files[index] = assert(io.open('/data/open-' .. index, 'w')) \
                     end \
                     local excess, excess_message = io.open('/data/open-17', 'w') \
                     assert(excess == nil and type(excess_message) == 'string')",
                )
                .await?;
            assert_eq!(
                filesystem.read("/workspace/media/result.txt").await?,
                b"result"
            );
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("read-only mounts and file limits");
    }

    #[test]
    fn lua_tmpfile_is_removed_on_explicit_close() {
        block_on(async {
            let filesystem = filesystem().await?;
            let mut installed = lua_with_file_package(filesystem.clone())?;
            installed
                .run(
                    "local file = assert(io.tmpfile()) \
                     assert(file:write('temporary')) \
                     assert(file:seek('set') == 0) \
                     assert(file:read('*a') == 'temporary') \
                     assert(file:close())",
                )
                .await?;
            assert!(filesystem.list_dir("/cache").await?.is_empty());
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("temporary file cleanup");
    }

    #[test]
    fn filesystem_os_subset_has_standard_shape_without_process_authority() {
        block_on(async {
            let filesystem = filesystem().await?;
            let mut installed = lua_with_file_package(filesystem.clone())?;
            installed
                .run(
                    "assert(os == require('os')) \
                     assert(type(os.remove) == 'function') \
                     assert(type(os.rename) == 'function') \
                     assert(type(os.tmpname) == 'function') \
                     assert(os.execute == nil and os.exit == nil and os.getenv == nil) \
                     local name = assert(os.tmpname()) \
                     local file = assert(io.open(name, 'w')) \
                     assert(file:write('value')) \
                     assert(file:close()) \
                     local crossed, crossed_message = os.rename(name, '/data/renamed.txt') \
                     assert(crossed == nil and type(crossed_message) == 'string') \
                     assert(os.rename(name, '/cache/renamed.txt')) \
                     assert(os.remove('/cache/renamed.txt')) \
                     local removed, message = os.remove('/cache/renamed.txt') \
                     assert(removed == nil and type(message) == 'string')",
                )
                .await?;
            assert!(!filesystem.exists("/cache/renamed.txt").await?);
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("filesystem-only os subset");
    }

    #[test]
    fn revoked_file_package_rejects_callbacks_already_installed_in_lua() {
        block_on(async {
            let filesystem = filesystem().await?;
            let package = FilePackage::new(filesystem);
            let mut lua = Lua::new()?;
            let _io = barracuda_vm_builtin_packages::BuiltinPackages::all().install(&mut lua)?;
            package.install(&mut lua)?;
            package.revoke();

            lua.run(
                "local file, message = io.open('/data/revoked.txt', 'w') \
                 assert(file == nil and type(message) == 'string')",
            )
            .await?;
            Ok::<_, Box<dyn core::error::Error>>(())
        })
        .expect("revoked vm-filesystem package");
    }

    mod barracuda_lua_test_fixture {
        use alloc::string::ToString;

        use barracuda_vm_plugin::{Package, Result};
        use barracuda_vm_runtime::FixedMemoryLua;

        pub(super) struct Installed {
            lua: Option<FixedMemoryLua>,
        }

        pub(super) fn install(package: impl Package) -> Result<Installed> {
            let mut lua = FixedMemoryLua::new(96 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io =
                barracuda_vm_builtin_packages::BuiltinPackages::all().install(lua.lua_mut())?;
            package.install(lua.lua_mut())?;
            Ok(Installed { lua: Some(lua) })
        }

        impl Installed {
            pub(super) async fn run(&mut self, source: &str) -> Result<()> {
                let lua = self
                    .lua
                    .take()
                    .ok_or_else(|| barracuda_vm_plugin::Error::runtime("Lua already executed"))?;
                let mut lua = lua;
                lua.lua_mut().load(source).exec_async().await
            }
        }
    }
}
