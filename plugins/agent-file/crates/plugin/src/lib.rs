//! Scoped file-operation Tools for the Agent runtime.

#![no_std]

extern crate alloc;

mod tools;

use barracuda_agent_plugin::AgentToolRegistry;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};

/// Plugin that gives Agents bounded access to the Plugin filesystem.
#[barracuda_plugin::macros::plugin]
pub struct AgentFilePlugin;

impl AgentFilePlugin {
    /// Creates the stateless Agent file adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentFilePlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let filesystem = context.filesystem()?.clone();
        let registry = context.require::<AgentToolRegistry>("agent")?;
        registry
            .register_group(tools::file_tools(filesystem))
            .map_err(PluginError::registration)
    }
}

#[cfg(test)]
mod tests {
    use barracuda_plugin::manager::{Plugin, PluginFilesystem};

    use super::AgentFilePlugin;

    #[test]
    fn declares_plugin_filesystem_access() {
        assert_eq!(
            AgentFilePlugin::REQUIREMENTS.filesystem(),
            PluginFilesystem::Private
        );
    }
}
