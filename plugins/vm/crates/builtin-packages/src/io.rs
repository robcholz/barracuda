//! Message-based input and output for sandboxed Barracuda Lua states.

use alloc::string::String;

use async_channel::{Receiver, Sender};
use barracuda_lua::{Error, Lua, Package, Result};

const INPUT_CAPACITY: usize = 1;
const OUTPUT_CAPACITY: usize = 1;
const INPUT_REQUEST_CAPACITY: usize = 1;

const INSTALL_PACKAGE: &str = r##"
local io = require("io")
local native_read = io.__read
local native_emit = io.__emit
io.__read = nil
io.__emit = nil

local input_buffer = ""
local input_eof = false

local function fill_input()
    if input_eof then
        return false
    end
    local value = native_read()
    if value == nil then
        input_eof = true
        return false
    end
    input_buffer = input_buffer .. value .. "\n"
    return true
end

local function read_line(include_newline)
    while true do
        local newline = string.find(input_buffer, "\n", 1, true)
        if newline ~= nil then
            local finish = include_newline and newline or newline - 1
            local value = string.sub(input_buffer, 1, finish)
            input_buffer = string.sub(input_buffer, newline + 1)
            return value
        end
        if not fill_input() then
            if input_buffer == "" then
                return nil
            end
            local value = input_buffer
            input_buffer = ""
            return value
        end
    end
end

local function read_count(count)
    if count < 0 or count % 1 ~= 0 then
        error("bad argument to 'read' (invalid format)", 3)
    end
    while #input_buffer < count and fill_input() do end
    if count == 0 then
        if input_buffer == "" and not fill_input() then
            return nil
        end
        return ""
    end
    if input_buffer == "" then
        return nil
    end
    local value = string.sub(input_buffer, 1, count)
    input_buffer = string.sub(input_buffer, #value + 1)
    return value
end

local function read_all()
    while fill_input() do end
    local value = input_buffer
    input_buffer = ""
    return value
end

local function read_number()
    while true do
        local first = string.find(input_buffer, "%S")
        if first ~= nil then
            input_buffer = string.sub(input_buffer, first)
            break
        end
        input_buffer = ""
        if not fill_input() then
            return nil
        end
    end
    while not string.find(input_buffer, "%s") and fill_input() do end
    local finish = string.find(input_buffer, "%s")
    local token
    if finish == nil then
        token = input_buffer
        input_buffer = ""
    else
        token = string.sub(input_buffer, 1, finish - 1)
        input_buffer = string.sub(input_buffer, finish)
    end
    return tonumber(token)
end

local function read_one(format)
    if type(format) == "number" then
        return read_count(format)
    end
    if format == "l" or format == "*l" then
        return read_line(false)
    end
    if format == "L" or format == "*L" then
        return read_line(true)
    end
    if format == "a" or format == "*a" then
        return read_all()
    end
    if format == "n" or format == "*n" then
        return read_number()
    end
    error("bad argument to 'read' (invalid format)", 3)
end

local stdin = {}
local stdout = {}
local stderr = {}

function stdin:read(...)
    local formats = table.pack(...)
    if formats.n == 0 then
        formats[1] = "l"
        formats.n = 1
    end
    local values = {}
    local count = 0
    for index = 1, formats.n do
        local value = read_one(formats[index])
        if value == nil then
            if count == 0 then
                return nil
            end
            break
        end
        count = count + 1
        values[count] = value
    end
    return table.unpack(values, 1, count)
end

function stdin:lines(...)
    local formats = table.pack(...)
    if formats.n == 0 then
        formats[1] = "l"
        formats.n = 1
    end
    return function()
        return self:read(table.unpack(formats, 1, formats.n))
    end
end

local function standard_stream_error()
    return nil, "cannot close standard file"
end

stdin.close = standard_stream_error
stdin.flush = function() return true end
stdin.seek = function() return nil, "standard input is not seekable" end
stdin.setvbuf = function() return true end

local function write_stream(stream, ...)
    local values = table.pack(...)
    local output = ""
    for index = 1, values.n do
        local kind = type(values[index])
        if kind ~= "string" and kind ~= "number" then
            error("bad argument #" .. index .. " to 'write' (string expected)", 3)
        end
        output = output .. tostring(values[index])
    end
    if output ~= "" then
        native_emit(output)
    end
    return stream
end

function stdout:write(...)
    return write_stream(self, ...)
end

function stderr:write(...)
    return write_stream(self, ...)
end

stdout.close = standard_stream_error
stderr.close = standard_stream_error
stdout.flush = function() return true end
stderr.flush = function() return true end
stdout.seek = function() return nil, "standard output is not seekable" end
stderr.seek = function() return nil, "standard error is not seekable" end
stdout.setvbuf = function() return true end
stderr.setvbuf = function() return true end

io.stdin = stdin
io.stdout = stdout
io.stderr = stderr

function io.read(...)
    return stdin:read(...)
end

function io.lines(...)
    local arguments = table.pack(...)
    if arguments.n == 0 then
        return stdin:lines()
    end
    if arguments[1] ~= nil then
        error("io.lines does not support files", 2)
    end
    return stdin:lines(table.unpack(arguments, 2, arguments.n))
end

function io.write(...)
    return stdout:write(...)
end

function io.flush()
    return stdout:flush()
end

function io.input(...)
    if select("#", ...) == 0 or ... == nil then
        return stdin
    end
    if ... == stdin then
        return stdin
    end
    error("io.input does not support files", 2)
end

function io.output(...)
    if select("#", ...) == 0 or ... == nil then
        return stdout
    end
    if ... == stdout then
        return stdout
    end
    error("io.output does not support files", 2)
end

function io.close(file)
    return (file or stdout):close()
end

function io.type(value)
    if value == stdin or value == stdout or value == stderr then
        return "file"
    end
    return nil
end

function print(...)
    local values = table.pack(...)
    local line = ""
    for index = 1, values.n do
        if index > 1 then line = line .. "\t" end
        line = line .. tostring(values[index])
    end
    stdout:write(line, "\n")
end

_G.io = io
"##;

/// The installable Lua IO package.
pub struct Io {
    input: Receiver<String>,
    input_requests: Sender<()>,
    output: Sender<String>,
}

impl Io {
    /// Creates the package and its independent host-side data flows.
    #[must_use]
    pub fn new() -> (Self, Input, Output) {
        let (input_sender, input_receiver) = async_channel::bounded(INPUT_CAPACITY);
        let (output_sender, output_receiver) = async_channel::bounded(OUTPUT_CAPACITY);
        let (request_sender, request_receiver) = async_channel::bounded(INPUT_REQUEST_CAPACITY);

        (
            Self {
                input: input_receiver,
                input_requests: request_sender,
                output: output_sender,
            },
            Input {
                sender: input_sender,
                requests: request_receiver,
            },
            Output {
                receiver: output_receiver,
            },
        )
    }
}

impl Package for Io {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let input = self.input.clone();
        let input_requests = self.input_requests.clone();
        let output = self.output.clone();
        lua.register_lib("io", move |package| {
            package.register_async("__read", move |(): ()| {
                let input = input.clone();
                let input_requests = input_requests.clone();
                async move {
                    if input_requests.send(()).await.is_err() {
                        return Some(Ok(None));
                    }
                    Some(Ok(input.recv().await.ok()))
                }
            })?;
            package.register_async("__emit", move |line: String| {
                let output = output.clone();
                async move {
                    let _ = output.send(line).await;
                    None::<Result<()>>
                }
            })
        })?;
        lua.load(INSTALL_PACKAGE).exec()
    }
}

/// Sends complete messages consumed by Lua `io.read()` calls.
#[derive(Clone)]
pub struct Input {
    sender: Sender<String>,
    requests: Receiver<()>,
}

impl Input {
    /// Sends one complete input message.
    pub async fn send(&self, input: impl Into<String>) -> Result<()> {
        self.sender
            .send(input.into())
            .await
            .map_err(|_| Error::runtime("Lua input is closed"))
    }

    /// Closes the input flow. Once queued messages are consumed, `io.read()`
    /// returns `nil`.
    pub fn close(&self) {
        self.sender.close();
    }

    /// Waits until Lua reaches one `io.read()` call.
    pub async fn next_request(&self) -> bool {
        self.requests.recv().await.is_ok()
    }
}

/// Receives output chunks emitted through Lua's virtual standard streams.
pub struct Output {
    receiver: Receiver<String>,
}

impl Output {
    /// Returns the next output message, or `None` after execution has stopped
    /// and all buffered output has been consumed.
    pub async fn next(&mut self) -> Option<String> {
        self.receiver.recv().await.ok()
    }
}
