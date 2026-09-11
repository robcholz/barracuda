//! Lua projection of the statically selected Platform and Board identity.

#![no_std]

use barracuda_plugin::api::{PluginContext, TargetIdentity};
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Lua, LuaPackage, LuaPackageRegistry, Package, Result};

/// Registers the require-only `systeminfo` package in every Lua VM.
#[barracuda_plugin::macros::plugin]
pub struct VmSystemInfoPlugin {
    target_identity: TargetIdentity,
}

impl VmSystemInfoPlugin {
    /// Copies the selected Target identity from the shared construction context.
    #[must_use]
    pub const fn new<Peripherals, ExposedIo>(
        context: &mut PluginContext<Peripherals, ExposedIo>,
    ) -> Self {
        Self {
            target_identity: context.target_identity,
        }
    }
}

impl Plugin for VmSystemInfoPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let registration = packages
            .register(SystemInfoPackage::new(self.target_identity))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct SystemInfoPackage {
    target_identity: TargetIdentity,
}

impl SystemInfoPackage {
    const fn new(target_identity: TargetIdentity) -> Self {
        Self { target_identity }
    }
}

impl Package for SystemInfoPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let platform = *self.target_identity.platform();
        let board = *self.target_identity.board();
        lua.register_lib("systeminfo", move |package| {
            package.table("platform", |table| {
                table.set("name", platform.name())?;
                table.set("family", platform.family())?;
                table.set("architecture", platform.architecture())?;
                table.set("environment", platform.environment())
            })?;
            package.table("board", |table| {
                table.set("name", board.name())?;
                table.set("chip", board.hardware().chip())
            })
        })
    }
}

impl LuaPackage for SystemInfoPackage {
    fn name(&self) -> &'static str {
        "systeminfo"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use barracuda_plugin::api::{BoardInfo, Hardware, PlatformInfo};
    use barracuda_plugin::manager::PluginDeclaration;

    use super::*;

    const INFO: TargetIdentity = TargetIdentity::new(
        PlatformInfo::new("test-platform", "test-family", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );

    fn lua_with_package() -> Lua {
        let mut lua = Lua::new().expect("create Lua");
        SystemInfoPackage::new(INFO)
            .install(&mut lua)
            .expect("install systeminfo package");
        lua
    }

    #[test]
    fn declaration_depends_only_on_vm() {
        assert_eq!(VmSystemInfoPlugin::ID, "vm-systeminfo");
        assert_eq!(VmSystemInfoPlugin::DEPENDS_ON, &["vm"]);
    }

    #[test]
    fn package_is_require_only_and_contains_both_identity_axes() {
        let mut lua = lua_with_package();

        let values: (
            std::string::String,
            std::string::String,
            std::string::String,
            std::string::String,
            std::string::String,
            std::string::String,
        ) = lua
            .load(
                "assert(systeminfo == nil)\n\
                 local info = require('systeminfo')\n\
                 return info.platform.name, info.platform.family,\n\
                        info.platform.architecture, info.platform.environment,\n\
                        info.board.name, info.board.chip",
            )
            .eval()
            .expect("read system info");

        assert_eq!(
            values,
            (
                "test-platform".into(),
                "test-family".into(),
                "test-arch".into(),
                "hosted".into(),
                "test-board".into(),
                "test-chip".into(),
            )
        );
    }

    #[test]
    fn each_lua_state_receives_an_independent_table() {
        let mut first = lua_with_package();
        let mut second = lua_with_package();

        first
            .load("require('systeminfo').board.name = 'changed'")
            .exec()
            .expect("mutate first state");
        let first_name: std::string::String = first
            .load("return require('systeminfo').board.name")
            .eval()
            .expect("read first state");
        let second_name: std::string::String = second
            .load("return require('systeminfo').board.name")
            .eval()
            .expect("read second state");

        assert_eq!(first_name, "changed");
        assert_eq!(second_name, "test-board");
    }
}
