use alloc::boxed::Box;
use core::future::pending;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_lua::Lua;
use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_package_api::VmPackageRegistry;
use getset::CopyGetters;

use crate::VmRuntime;
use crate::run::{Run, run_handler_with_packages};
use crate::runtime::task_run_handler;

/// Default maximum Lua source size accepted by one `vm.run` call.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 65_536;
/// Default maximum size of one logical `io.input()` message.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 4_096;
/// Default instruction interval between cooperative executor yields.
pub const DEFAULT_INSTRUCTION_HOOK_INTERVAL: u32 = 10_000;

pub(crate) fn create_lua() -> barracuda_lua::Result<Lua> {
    Lua::new()
}

/// Per-call protocol and execution limits enforced by `vm.run`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct VmLimits {
    /// Maximum UTF-8 byte length of the complete Lua source.
    #[getset(get_copy = "pub")]
    max_source_bytes: usize,
    /// Maximum UTF-8 byte length of one complete `io.input()` message.
    #[getset(get_copy = "pub")]
    max_input_bytes: usize,
    /// Lua instruction count between cooperative executor yields.
    #[getset(get_copy = "pub")]
    instruction_hook_interval: u32,
}

impl VmLimits {
    /// Creates explicit source and input-message limits.
    #[must_use]
    pub const fn new(max_source_bytes: usize, max_input_bytes: usize) -> Self {
        Self {
            max_source_bytes,
            max_input_bytes,
            instruction_hook_interval: DEFAULT_INSTRUCTION_HOOK_INTERVAL,
        }
    }

    /// Replaces the instruction interval between cooperative VM yields.
    #[must_use]
    pub const fn with_instruction_hook_interval(mut self, instruction_hook_interval: u32) -> Self {
        self.instruction_hook_interval = instruction_hook_interval;
        self
    }
}

impl Default for VmLimits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_INPUT_BYTES)
    }
}

/// Event Router Component exposing the `vm.run` RPC.
pub struct VmComponent {
    limits: VmLimits,
    builtin_packages: BuiltinPackages,
    runtime: Option<VmRuntime>,
    external_packages: VmPackageRegistry,
}

impl VmComponent {
    /// Creates the Component from the package plan prepared by the VM Plugin.
    #[must_use]
    pub fn new(builtin_packages: BuiltinPackages) -> Self {
        Self {
            limits: VmLimits::new(DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_INPUT_BYTES),
            builtin_packages,
            runtime: None,
            external_packages: VmPackageRegistry::default(),
        }
    }

    /// Creates the Component with execution dispatched onto the VM Embassy task pool.
    #[must_use]
    pub fn with_runtime(builtin_packages: BuiltinPackages, runtime: VmRuntime) -> Self {
        Self {
            limits: VmLimits::new(DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_INPUT_BYTES),
            builtin_packages,
            runtime: Some(runtime),
            external_packages: VmPackageRegistry::default(),
        }
    }

    /// Creates the Component with the externally published package registry.
    #[must_use]
    pub fn with_runtime_and_packages(
        builtin_packages: BuiltinPackages,
        runtime: VmRuntime,
        external_packages: VmPackageRegistry,
    ) -> Self {
        Self {
            limits: VmLimits::new(DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_INPUT_BYTES),
            builtin_packages,
            runtime: Some(runtime),
            external_packages,
        }
    }

    /// Replaces the limits used by this Component.
    #[must_use]
    pub fn with_limits(mut self, limits: VmLimits) -> Self {
        self.limits = limits;
        self
    }
}

impl<const M: usize> Component<M> for VmComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        match &self.runtime {
            Some(runtime) => context.register_rpc::<Run, _>(task_run_handler(
                runtime.clone(),
                self.limits,
                self.builtin_packages,
                self.external_packages.clone(),
            )),
            None => context.register_rpc::<Run, _>(run_handler_with_packages(
                self.limits,
                self.builtin_packages,
                self.external_packages.clone(),
            )),
        }
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

    use super::create_lua;

    #[test]
    fn creates_a_sandboxed_lua_state() {
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
