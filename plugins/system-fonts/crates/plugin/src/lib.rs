//! The system fonts: the atlases every view on a display draws with for the
//! font slots it ships none for.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, rc::Rc, vec::Vec};
use barracuda_app_manager_plugin::{FontAtlases, SystemFonts};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_vfs::ScopedVfs;
use core::{future::Future, pin::Pin};

/// Where the atlases are in this Plugin's filesystem.
const FONTS: &str = "/resources/fonts";

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
        fonts.load_from(Rc::new(Atlases(context.filesystem()?.clone())));
        Ok(())
    }
}

/// Reads the atlases from this Plugin's own filesystem.
struct Atlases(ScopedVfs);

impl FontAtlases for Atlases {
    fn load(&self) -> Pin<Box<dyn Future<Output = Vec<Vec<u8>>> + '_>> {
        Box::pin(async move {
            let mut names = match self.0.list_dir(FONTS).await {
                Ok(names) => names,
                Err(error) => {
                    log::warn!("system fonts: none under {FONTS}: {error:?}");
                    return Vec::new();
                }
            };
            names.retain(|name| name.starts_with("font-") && name.ends_with(".bin"));
            names.sort_unstable();
            let mut atlases = Vec::with_capacity(names.len());
            for name in names {
                match self.0.read(&format!("{FONTS}/{name}")).await {
                    Ok(atlas) => atlases.push(atlas),
                    Err(error) => log::warn!("system fonts: cannot read {name}: {error:?}"),
                }
            }
            atlases
        })
    }
}
