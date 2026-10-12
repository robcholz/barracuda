//! The portal's file browser: the Workspace to read and change, and every
//! Plugin's private files to read.

#![no_std]

extern crate alloc;

mod api;
mod path;

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginRegisterContext,
    PluginRequirements, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::WebServer;

const ENTRY: WebEntry = WebEntry {
    id: "files",
    group: WebGroup::Device,
    order: 20,
    title: WebText {
        zh: "文件",
        en: "Files",
    },
    summary: WebText {
        zh: "查看 Agent 与脚本共用的文件",
        en: "See the files the agent and scripts share",
    },
    icon: Some("icon.svg"),
    figure: Some("figure.js"),
    module: "entry.js",
};

/// Plugin serving the portal's Files page and its `/api/files` routes.
#[barracuda_plugin::macros::plugin]
pub struct FilesPlugin;

impl FilesPlugin {
    /// Constructs the file browser; it owns no hardware and no task.
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for FilesPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Inspect);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let webserver = context.require::<WebServer>(Self::DEPENDS_ON[0])?;
        let portal = context.require::<CaptivePortal>(Self::DEPENDS_ON[1])?;
        let filesystem = context.filesystem()?.clone();
        let entry = portal
            .register(ENTRY, ResourceFiles::from(filesystem.clone()))
            .map_err(PluginError::registration)?;
        context.retain(entry);
        let routes = api::register(&webserver, &filesystem).map_err(PluginError::registration)?;
        context.retain(routes);
        Ok(())
    }
}
