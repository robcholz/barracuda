//! The system fonts: the atlases every view on a display draws with for the
//! font slots it ships none for.

#![no_std]

use barracuda_app_manager_plugin::SystemFonts;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};

/// Ships the system fonts in its `/resources/fonts` and hands them to the
/// `app-manager` Plugin's UI kit.
#[barracuda_plugin::macros::plugin]
pub struct SystemFontsPlugin;

impl SystemFontsPlugin {
    /// Creates the unregistered font set.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for SystemFontsPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let fonts = context.require::<SystemFonts>("app-manager")?;
        fonts.read_from(context.filesystem()?.clone());
        Ok(())
    }
}
