//! Message-based input and output for sandboxed Barracuda Lua states.

use alloc::string::String;

use async_channel::{Receiver, Sender};
use barracuda_lua::{Error, Lua, Package, Result};

const INPUT_CAPACITY: usize = 16;
const OUTPUT_CAPACITY: usize = 16;

const INSTALL_PACKAGE: &str = r##"
local io = require("io")
local emit = io.__emit
io.__emit = nil

function io.print(...)
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

/// The installable Lua IO package.
pub struct Io {
    input: Receiver<String>,
    output: Sender<String>,
}

impl Io {
    /// Creates the package and its independent host-side data flows.
    #[must_use]
    pub fn new() -> (Self, Input, Output) {
        let (input_sender, input_receiver) = async_channel::bounded(INPUT_CAPACITY);
        let (output_sender, output_receiver) = async_channel::bounded(OUTPUT_CAPACITY);

        (
            Self {
                input: input_receiver,
                output: output_sender,
            },
            Input {
                sender: input_sender,
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
        let output = self.output.clone();
        lua.register_lib("io", move |package| {
            package.register_async("input", move |(): ()| {
                let input = input.clone();
                async move { Some(Ok(input.recv().await.ok())) }
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

/// Sends complete messages consumed by Lua `io.input()` calls.
#[derive(Clone)]
pub struct Input {
    sender: Sender<String>,
}

impl Input {
    /// Sends one complete input message.
    pub async fn send(&self, input: impl Into<String>) -> Result<()> {
        self.sender
            .send(input.into())
            .await
            .map_err(|_| Error::runtime("Lua input is closed"))
    }

    /// Closes the input flow. Once queued messages are consumed, `io.input()`
    /// returns `nil`.
    pub fn close(&self) {
        self.sender.close();
    }
}

/// Receives complete messages emitted by Lua `io.print()`.
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
