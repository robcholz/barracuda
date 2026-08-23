use alloc::boxed::Box;
use core::future::pending;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_lua::Lua;
use getset::CopyGetters;

use crate::run::{Run, run_handler};

/// Default maximum Lua source size accepted by one `vm.run` call.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 65_536;
/// Default maximum size of one logical `io.input()` message.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 4_096;

fn configure_lua(_lua: &mut Lua) -> barracuda_lua::Result<()> {
    // Register allowlisted native modules here. Lua::new() is already sandboxed.
    Ok(())
}

pub(crate) fn create_lua() -> barracuda_lua::Result<Lua> {
    let mut lua = Lua::new()?;
    configure_lua(&mut lua)?;
    Ok(lua)
}

/// Per-call allocation limits enforced by the `vm.run` protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct VmLimits {
    /// Maximum UTF-8 byte length of the complete Lua source.
    #[getset(get_copy = "pub")]
    max_source_bytes: usize,
    /// Maximum UTF-8 byte length of one complete `io.input()` message.
    #[getset(get_copy = "pub")]
    max_input_bytes: usize,
}

impl VmLimits {
    /// Creates explicit source and input-message limits.
    #[must_use]
    pub const fn new(max_source_bytes: usize, max_input_bytes: usize) -> Self {
        Self {
            max_source_bytes,
            max_input_bytes,
        }
    }
}

impl Default for VmLimits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_INPUT_BYTES)
    }
}

/// Event Router Component exposing the `vm.run` RPC.
#[derive(Default)]
pub struct VmComponent {
    limits: VmLimits,
}

impl<const M: usize> Component<M> for VmComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<Run, _>(run_handler(self.limits))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use barracuda_lua::Lua;

    use super::{VmComponent, create_lua};

    #[test]
    fn default_component_creates_configured_lua() {
        let _component = VmComponent::default();
        create_lua().expect("create configured Lua");
    }

    #[test]
    fn default_lua_exposes_only_allowlisted_capabilities() {
        let mut lua = create_lua().expect("create configured Lua");
        let sandboxed: bool = lua
            .load(
                "return package == nil and io == nil and os == nil and debug == nil \
                 and load == nil and loadfile == nil and dofile == nil \
                 and collectgarbage == nil and warn == nil and print == nil \
                 and getmetatable == nil and setmetatable == nil \
                 and rawget == nil and rawset == nil and rawlen == nil and rawequal == nil \
                 and type(require) == 'function' and type(pcall) == 'function' \
                 and type(tostring) == 'function' and type(select) == 'function'",
            )
            .eval()
            .expect("inspect sandbox globals");

        assert!(sandboxed);
    }

    #[test]
    fn default_lua_require_resolves_only_registered_modules() {
        let mut lua = Lua::new().expect("create Lua");
        lua.register_lib("allowed", |library| library.set("version", 1_i64))
            .expect("register allowed module");

        let allowlisted_only: bool = lua
            .load(
                "local allowed = require('allowed') \
                 local unknown_loaded = pcall(require, 'unknown') \
                 return allowed.version == 1 and not unknown_loaded and package == nil",
            )
            .eval()
            .expect("evaluate require policy");

        assert!(allowlisted_only);
    }
}
