//! The Files system app, and the `files` Lua package that counts and lists
//! the files in the shared Workspace.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec, vec::Vec};
use barracuda_app_manager_plugin::{AppManager, BuiltinApp, BuiltinBackend, BuiltinPackage};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_vfs::ScopedVfs;
use core::{future::Future, pin::Pin};
use serde_json::{Value, json};

/// The app's ID, and its package's name.
pub const FILES: &str = "files";
/// The shared areas the app shows: its name, and the directory.
pub const AREAS: [(&str, &str); 4] = [
    ("Media", "/workspace/media"),
    ("Resources", "/workspace/resources"),
    ("SD card", "/workspace/removable"),
    ("Cache", "/workspace/cache"),
];
/// The most files a listing holds.
pub const MAX_FILES: usize = 64;
/// The deepest directory a walk enters.
const MAX_DEPTH: usize = 8;

/// Ships the Files app and answers its `files` package.
#[barracuda_plugin::macros::plugin]
pub struct FilesAppPlugin;

impl FilesAppPlugin {
    /// Creates the unregistered app.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for FilesAppPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let apps = context.require::<AppManager>("app-manager")?;
        let filesystem = context.filesystem()?.clone();
        let registration = apps
            .add_builtin(BuiltinApp {
                id: FILES,
                filesystem: filesystem.clone(),
                opens: &[],
                package: Some(BuiltinPackage {
                    name: FILES,
                    backend: Rc::new(Files(filesystem)),
                }),
            })
            .map_err(PluginError::registration)?;
        if let Some(registration) = registration {
            context.retain(registration);
        }
        Ok(())
    }
}

/// Answers the `files` package from this Plugin's view of the Workspace.
struct Files(ScopedVfs);

impl BuiltinBackend for Files {
    fn call<'a>(
        &'a self,
        call: &'a str,
        args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            match call {
                "areas" => {
                    let mut areas = Vec::new();
                    for (name, root) in AREAS {
                        let usage = walk(&self.0, root, None).await;
                        areas.push(json!({
                            "name": name,
                            "count": usage.files,
                            "present": usage.present,
                        }));
                    }
                    Ok(Value::Array(areas))
                }
                "list" => {
                    let wanted = args.first().cloned().flatten().unwrap_or_default();
                    let (name, root) = AREAS
                        .into_iter()
                        .find(|(name, _)| *name == wanted)
                        .ok_or_else(|| format!("no file area \"{wanted}\""))?;
                    let mut files = Vec::new();
                    let usage = walk(&self.0, root, Some(&mut files)).await;
                    Ok(json!({
                        "name": name,
                        "present": usage.present,
                        "count": usage.files,
                        "label": size(usage.bytes),
                        "files": files
                            .into_iter()
                            .map(|(path, bytes)| json!({ "path": path, "label": size(bytes) }))
                            .collect::<Vec<_>>(),
                    }))
                }
                _ => Err(format!("files.{call} does not exist")),
            }
        })
    }
}

/// What a directory tree holds.
#[derive(Default)]
struct Usage {
    present: bool,
    files: u64,
    bytes: u64,
}

/// Adds up the files under `root`, listing up to [`MAX_FILES`] of them into
/// `list` by their path under it, sorted.
async fn walk(
    filesystem: &ScopedVfs,
    root: &str,
    mut list: Option<&mut Vec<(String, u64)>>,
) -> Usage {
    let mut usage = Usage::default();
    let mut pending = vec![(String::from(root), 0_usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = filesystem.read_dir(&dir).await else {
            continue;
        };
        usage.present = true;
        for entry in entries.flatten() {
            let path = format!("{dir}/{}", entry.file_name());
            let metadata = entry.metadata();
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if metadata.is_file() {
                usage.files += 1;
                usage.bytes += metadata.len();
                if let Some(list) = list.as_deref_mut()
                    && list.len() < MAX_FILES
                {
                    let shown = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .trim_start_matches('/');
                    list.push((String::from(shown), metadata.len()));
                }
            }
        }
    }
    if let Some(list) = list {
        list.sort_unstable();
    }
    usage
}

/// A byte count in the largest unit that keeps it above one.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_their_unit() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(373 * 1024), "373 KB");
    }
}
