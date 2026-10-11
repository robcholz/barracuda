//! App bundles on the Plugin filesystem.
//!
//! A bundle is a directory named after its app ID:
//!
//! ```text
//! <id>/app.json      {"id": "<id>", "name": "…", "version": "…"}
//! <id>/main.lua      the program
//! <id>/view.bvir     optional compiled view, with styles.bin and font-<slot>.bin
//! ```

use crate::AppError;
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use barracuda_ui_plugin::UiAssets;
use barracuda_vfs::{FsError, ScopedVfs};
use serde::{Deserialize, Serialize};

/// Bundles shipped in the system image, inside this Plugin's private resources.
pub const BUNDLED_APPS: &str = "/resources/apps";
/// Bundles installed at runtime, inside this Plugin's private media.
pub const INSTALLED_APPS: &str = "/media/apps";

/// The largest `main.lua` an app may ship.
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024;
/// The largest combined view, stylesheet, and font size of one app.
pub const MAX_VIEW_BYTES: u64 = 512 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_ID_LEN: usize = 32;

/// Where an app's bundle lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLocation {
    /// Shipped in the system image under [`BUNDLED_APPS`].
    Bundled,
    /// Installed under [`INSTALLED_APPS`]; shadows a bundled app with the same ID.
    Installed,
}

/// One launchable app.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AppInfo {
    /// Stable ID, equal to the bundle directory name.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Bundle version as written by its author.
    pub version: String,
    /// Where the bundle was found.
    pub location: AppLocation,
    /// Whether the app has a view; apps without one run headless.
    pub has_view: bool,
}

#[derive(Deserialize)]
struct Manifest {
    id: String,
    name: String,
    version: String,
}

pub(crate) struct Bundle {
    pub(crate) info: AppInfo,
    pub(crate) source: String,
    pub(crate) view: Option<UiAssets>,
}

/// App IDs are short lowercase names, safe in paths and Lua string literals.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn root(location: AppLocation) -> &'static str {
    match location {
        AppLocation::Bundled => BUNDLED_APPS,
        AppLocation::Installed => INSTALLED_APPS,
    }
}

async fn read_limited(filesystem: &ScopedVfs, path: &str, limit: u64) -> Result<Vec<u8>, AppError> {
    let len = filesystem.len(path).await.map_err(storage)?;
    if len > limit {
        return Err(AppError::InvalidBundle);
    }
    filesystem.read(path).await.map_err(storage)
}

fn storage(error: FsError) -> AppError {
    match error {
        FsError::NotFound => AppError::InvalidBundle,
        _ => AppError::Storage,
    }
}

async fn read_info(
    filesystem: &ScopedVfs,
    location: AppLocation,
    id: &str,
) -> Result<AppInfo, AppError> {
    let dir = format!("{}/{id}", root(location));
    let manifest = read_limited(filesystem, &format!("{dir}/app.json"), MAX_MANIFEST_BYTES).await?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest).map_err(|_| AppError::InvalidBundle)?;
    if manifest.id != id
        || !filesystem
            .exists(&format!("{dir}/main.lua"))
            .await
            .unwrap_or(false)
    {
        return Err(AppError::InvalidBundle);
    }
    Ok(AppInfo {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        location,
        has_view: filesystem
            .exists(&format!("{dir}/view.bvir"))
            .await
            .unwrap_or(false),
    })
}

/// Every valid app, sorted by ID. Installed bundles shadow bundled ones.
pub(crate) async fn list(filesystem: &ScopedVfs) -> Vec<AppInfo> {
    let mut apps = BTreeMap::new();
    for location in [AppLocation::Bundled, AppLocation::Installed] {
        let names = match filesystem.list_dir(root(location)).await {
            Ok(names) => names,
            Err(FsError::NotFound | FsError::NotMounted) => continue,
            Err(error) => {
                log::warn!("app-manager: cannot list {}: {error:?}", root(location));
                continue;
            }
        };
        for name in names.into_iter().filter(|name| valid_id(name)) {
            match read_info(filesystem, location, &name).await {
                Ok(info) => {
                    apps.insert(name, info);
                }
                Err(error) => {
                    log::warn!("app-manager: skipping {}/{name}: {error}", root(location));
                }
            }
        }
    }
    apps.into_values().collect()
}

/// Reads everything needed to launch app `id`.
pub(crate) async fn load(filesystem: &ScopedVfs, id: &str) -> Result<Bundle, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    let mut location = None;
    for candidate in [AppLocation::Installed, AppLocation::Bundled] {
        let manifest = format!("{}/{id}/app.json", root(candidate));
        if filesystem.exists(&manifest).await.unwrap_or(false) {
            location = Some(candidate);
            break;
        }
    }
    let location = location.ok_or(AppError::NotFound)?;
    let info = read_info(filesystem, location, id).await?;
    let dir = format!("{}/{id}", root(location));
    let source = read_limited(filesystem, &format!("{dir}/main.lua"), MAX_SOURCE_BYTES).await?;
    let source = String::from_utf8(source).map_err(|_| AppError::InvalidBundle)?;
    let view = if info.has_view {
        Some(load_view(filesystem, &dir).await?)
    } else {
        None
    };
    Ok(Bundle { info, source, view })
}

async fn load_view(filesystem: &ScopedVfs, dir: &str) -> Result<UiAssets, AppError> {
    let mut budget = MAX_VIEW_BYTES;
    let mut take = async |name: &str| -> Result<Vec<u8>, AppError> {
        let bytes = read_limited(filesystem, &format!("{dir}/{name}"), budget).await?;
        budget = budget
            .checked_sub(bytes.len() as u64)
            .ok_or(AppError::InvalidBundle)?;
        Ok(bytes)
    };
    let view = take("view.bvir").await?;
    let styles = take("styles.bin").await?;
    let mut fonts = Vec::new();
    let mut names = filesystem.list_dir(dir).await.map_err(storage)?;
    names.sort_unstable();
    for name in names {
        if name.starts_with("font-") && name.ends_with(".bin") {
            fonts.push(take(&name).await?);
        }
    }
    Ok(UiAssets {
        view,
        styles,
        fonts,
    })
}
