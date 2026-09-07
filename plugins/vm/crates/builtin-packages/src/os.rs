//! Empty standard `os` table extended by capability Plugins.

use barracuda_lua::{Lua, Package, Result};

const INSTALL_PACKAGE: &str = r#"_G.os = require("os")"#;

/// VM-owned base for the sandboxed standard `os` library.
pub struct Os;

impl Package for Os {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        lua.register_lib("os", |_package| Ok(()))?;
        lua.load(INSTALL_PACKAGE).exec()
    }
}
