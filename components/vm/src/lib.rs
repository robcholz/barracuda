//! Script VM integration built on the shared `barracuda-lua` binding crate.
#![no_std]

use barracuda_lua::Lua;

pub type Result<T> = barracuda_lua::Result<T>;

/// Owns the language runtime used by the VM component.
pub struct Vm {
    lua: Lua,
}

impl Vm {
    pub fn new() -> Result<Self> {
        Ok(Self { lua: Lua::new()? })
    }

    pub fn lua_mut(&mut self) -> &mut Lua {
        &mut self.lua
    }

    pub fn into_lua(self) -> Lua {
        self.lua
    }
}
