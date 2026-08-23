use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::runtime::ExecutionState;
use crate::{Error, Lua, Result};

impl Lua {
    /// Executes one already-configured sandbox.
    ///
    /// This consumes the Lua state because [`LuaExecution`] owns it until the
    /// script finishes or the execution future is dropped. Script load and
    /// runtime failures are reported by `LuaExecution`.
    #[must_use]
    pub fn run(mut self, source: &str) -> LuaExecution {
        let state = self.start_owned_ignoring(source.as_bytes());
        LuaExecution {
            state,
            lua: Some(self),
        }
    }
}

/// Drives one isolated Lua script and resolves to its final run result.
pub struct LuaExecution {
    state: ExecutionState<()>,
    lua: Option<Lua>,
}

impl Unpin for LuaExecution {}

impl Future for LuaExecution {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let Some(lua) = this.lua.as_mut() else {
            return Poll::Ready(Err(Error::runtime("Lua execution has completed")));
        };
        let result = this.state.poll(lua, context);
        if result.is_ready() {
            this.lua.take();
        }
        result
    }
}

impl Drop for LuaExecution {
    fn drop(&mut self) {
        if let Some(mut lua) = self.lua.take() {
            self.state.cleanup(&mut lua);
        }
    }
}
