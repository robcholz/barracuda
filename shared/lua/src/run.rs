use alloc::string::String;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use async_channel::{Receiver, Sender};

use crate::runtime::ExecutionState;
use crate::{Error, Lua, Result};

const INPUT_CAPACITY: usize = 16;
const OUTPUT_CAPACITY: usize = 16;

const INSTALL_PRINT: &str = r##"
local emit = __barracuda_emit_print
__barracuda_emit_print = nil

function print(...)
    local count = select("#", ...)
    local line = ""
    for index = 1, count do
        if index > 1 then
            line = line .. "\t"
        end
        line = line .. tostring(select(index, ...))
    end
    emit(line)
end
"##;

impl Lua {
    /// Creates one isolated script execution and its two message flows.
    ///
    /// This consumes the Lua state because [`LuaExecution`] owns it until the
    /// script finishes or the execution future is dropped. Script load and
    /// runtime failures are reported by `LuaExecution`.
    pub fn run(mut self, source: &str) -> Result<(LuaInput, LuaOutput, LuaExecution)> {
        let (input_sender, input_receiver) = async_channel::bounded(INPUT_CAPACITY);
        let (output_sender, output_receiver) = async_channel::bounded(OUTPUT_CAPACITY);

        install_input(&mut self, input_receiver.clone())?;
        install_output(&mut self, output_sender.clone())?;

        let state = self.start_owned_ignoring(source.as_bytes());
        let input = LuaInput {
            sender: input_sender,
        };
        let output = LuaOutput {
            receiver: output_receiver,
        };
        let execution = LuaExecution {
            state,
            lua: self,
            input_receiver,
            output_sender,
        };
        Ok((input, output, execution))
    }
}

fn install_input(lua: &mut Lua, receiver: Receiver<String>) -> Result<()> {
    lua.register_async("input", move |(): ()| {
        let receiver = receiver.clone();
        async move { Some(Ok(receiver.recv().await.ok())) }
    })
}

fn install_output(lua: &mut Lua, sender: Sender<String>) -> Result<()> {
    lua.register_async("__barracuda_emit_print", move |line: String| {
        let sender = sender.clone();
        async move {
            let _ = sender.send(line).await;
            None::<Result<()>>
        }
    })?;
    lua.load(INSTALL_PRINT).exec()
}

/// Sends one message to the script. One message is consumed by one `input()`
/// call in Lua.
#[derive(Clone)]
pub struct LuaInput {
    sender: Sender<String>,
}

impl LuaInput {
    pub async fn send(&self, input: impl Into<String>) -> Result<()> {
        self.sender
            .send(input.into())
            .await
            .map_err(|_| Error::runtime("Lua input is closed"))
    }

    /// Closes the input flow. After queued messages are consumed, `input()`
    /// returns `nil`.
    pub fn close(&self) {
        self.sender.close();
    }
}

/// Receives complete messages emitted by Lua `print(...)` calls.
pub struct LuaOutput {
    receiver: Receiver<String>,
}

impl LuaOutput {
    /// Returns the next printed line, or `None` after execution has stopped and
    /// all buffered output has been consumed.
    pub async fn next(&mut self) -> Option<String> {
        self.receiver.recv().await.ok()
    }
}

/// Drives one isolated Lua script and resolves to its final run result.
pub struct LuaExecution {
    state: ExecutionState<()>,
    lua: Lua,
    input_receiver: Receiver<String>,
    output_sender: Sender<String>,
}

impl Unpin for LuaExecution {}

impl LuaExecution {
    fn close_data_flows(&self) {
        self.input_receiver.close();
        self.output_sender.close();
    }
}

impl Future for LuaExecution {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = this.state.poll(&mut this.lua, context);
        if result.is_ready() {
            this.close_data_flows();
        }
        result
    }
}

impl Drop for LuaExecution {
    fn drop(&mut self) {
        self.state.cleanup(&mut self.lua);
        self.close_data_flows();
    }
}
