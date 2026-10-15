//! App containers: one directory per app, in the format `docs/app-format.md`
//! defines and `resources/app-manifest.json` checks.
//!
//! Where a container lives decides what kind of app it is: the image's
//! bundled containers are system apps, and installed ones are installable
//! apps. Workspace containers are not launchable; they are what an install
//! copies into [`INSTALLED_APPS`].

use crate::AppError;
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use barracuda_ui_plugin::UiAssets;
use barracuda_vfs::{FsError, ScopedVfs};
use json_validator::JsonSchema;
use serde::{Deserialize, Serialize};

/// System apps, shipped in the system image inside this Plugin's private
/// resources.
pub const BUNDLED_APPS: &str = "/resources/apps";
/// Installable apps the person installed, inside this Plugin's private media.
pub const INSTALLED_APPS: &str = "/media/apps";
/// Containers that Agents, the portal, and other Plugins offer for
/// installation, in the shared Workspace.
pub const WORKSPACE_APPS: &str = "/workspace/media/apps";
/// The most Lua packages a manifest may ask for.
pub const MAX_PERMISSIONS: usize = 16;
/// The package every app program uses, granted without being asked for.
pub const APP_PACKAGE: &str = "app";
/// The package that runs the device's settings, granted only to system apps:
/// an installable app that asks for it is not valid.
pub const SYSTEM_PACKAGE: &str = "system";

/// The manifest file at the root of every container.
pub const MANIFEST_FILE: &str = "app.json";
/// The manifest format this Plugin reads.
pub const FORMAT_VERSION: u32 = 1;
/// The program a manifest runs when it names no `entry`.
pub const DEFAULT_ENTRY: &str = "main.lua";
/// The largest program an app may ship.
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024;
/// The largest combined view program, stylesheet, and font size of one app.
pub const MAX_VIEW_BYTES: u64 = 512 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_ID_LEN: usize = 32;

const MANIFEST_SCHEMA: JsonSchema = json_validator::json_schema!("resources/app-manifest.json");

/// Where an app's container lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLocation {
    /// Shipped in the system image under [`BUNDLED_APPS`].
    Bundled,
    /// Installed under [`INSTALLED_APPS`].
    Installed,
    /// Offered for installation in the Workspace under [`WORKSPACE_APPS`].
    Workspace,
}

/// What an app may do, decided by where its container lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    /// Shipped in the image: may require every Lua package, cannot be
    /// uninstalled, and no installed app may take its ID.
    System,
    /// Installed by the person: requires only `app` and the packages its
    /// manifest asked for when it was installed, and can be uninstalled.
    Installable,
}

impl AppLocation {
    /// The launchable locations, in the order an app ID is looked up: a system
    /// app's ID is never taken by an installed app.
    const LAUNCHABLE: [AppLocation; 2] = [AppLocation::Bundled, AppLocation::Installed];

    /// The kind of app a container here is.
    #[must_use]
    pub const fn kind(self) -> AppKind {
        match self {
            AppLocation::Bundled => AppKind::System,
            AppLocation::Installed | AppLocation::Workspace => AppKind::Installable,
        }
    }

    /// The directory holding this location's containers.
    #[must_use]
    pub const fn root(self) -> &'static str {
        match self {
            AppLocation::Bundled => BUNDLED_APPS,
            AppLocation::Installed => INSTALLED_APPS,
            AppLocation::Workspace => WORKSPACE_APPS,
        }
    }
}

/// One app container that passed the app format.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AppInfo {
    /// Stable ID, equal to the container directory name.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version as written by its author.
    pub version: String,
    /// Where the container was found.
    pub location: AppLocation,
    /// What the app may do.
    pub kind: AppKind,
    /// Whether the app has a view; apps without one run headless.
    pub has_view: bool,
    /// The Lua packages the manifest asks for besides `app`. Only an
    /// installable app is limited to them.
    pub permissions: Vec<String>,
    /// The name of the system icon the app is shown with, if it picked one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A container that cannot be launched, and why.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct InvalidApp {
    /// Where the container was found.
    pub location: AppLocation,
    /// The container directory name.
    pub dir: String,
    /// What is wrong, in terms of the app format.
    pub problem: String,
}

/// Every container, split into launchable, offered, and broken ones.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct AppList {
    /// Launchable system and installed apps, sorted by ID.
    pub apps: Vec<AppInfo>,
    /// Workspace containers that can be installed: new apps and installed
    /// apps at another version, sorted by ID.
    pub installable: Vec<AppInfo>,
    /// Containers that fail the app format, so their authors can fix them.
    pub invalid: Vec<InvalidApp>,
}

#[derive(Deserialize)]
struct Manifest {
    format: u32,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    view: Option<ViewFiles>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    icon: Option<String>,
}

#[derive(Deserialize)]
struct ViewFiles {
    program: String,
    styles: String,
    #[serde(default)]
    fonts: Vec<String>,
}

/// A container checked against the app format.
pub(crate) struct Container {
    pub(crate) info: AppInfo,
    pub(crate) dir: String,
    entry: String,
    view: Option<ViewFiles>,
}

impl Container {
    /// Every file the container consists of: the manifest and what it names.
    pub(crate) fn files(&self) -> Vec<&str> {
        let mut files = alloc::vec![MANIFEST_FILE, self.entry.as_str()];
        if let Some(view) = &self.view {
            files.push(&view.program);
            files.push(&view.styles);
            files.extend(view.fonts.iter().map(String::as_str));
        }
        files
    }
}

pub(crate) struct Bundle {
    pub(crate) info: AppInfo,
    pub(crate) source: String,
    pub(crate) view: Option<UiAssets>,
}

enum Problem {
    Storage,
    Invalid(String),
}

impl From<Problem> for AppError {
    fn from(problem: Problem) -> Self {
        match problem {
            Problem::Storage => AppError::Storage,
            Problem::Invalid(_) => AppError::InvalidBundle,
        }
    }
}

fn invalid(message: impl Into<String>) -> Problem {
    Problem::Invalid(message.into())
}

/// App IDs are short lowercase names, safe in paths and Lua string literals.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Files a manifest names sit directly in the container.
fn valid_file(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

async fn read_limited(
    filesystem: &ScopedVfs,
    dir: &str,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, Problem> {
    let path = format!("{dir}/{name}");
    let len = match filesystem.len(&path).await {
        Ok(len) => len,
        Err(FsError::NotFound) => return Err(invalid(format!("{name} is missing"))),
        Err(FsError::IsDirectory) => return Err(invalid(format!("{name} is not a file"))),
        Err(_) => return Err(Problem::Storage),
    };
    if len > limit {
        return Err(invalid(format!("{name} is larger than {limit} bytes")));
    }
    filesystem.read(&path).await.map_err(|_| Problem::Storage)
}

async fn require_file(filesystem: &ScopedVfs, dir: &str, name: &str) -> Result<(), Problem> {
    match filesystem.metadata(&format!("{dir}/{name}")).await {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(invalid(format!("{name} is not a file"))),
        Err(FsError::NotFound) => Err(invalid(format!("{name} is missing"))),
        Err(_) => Err(Problem::Storage),
    }
}

async fn read_container(
    filesystem: &ScopedVfs,
    location: AppLocation,
    name: &str,
) -> Result<Container, Problem> {
    if !valid_id(name) {
        return Err(invalid(
            "the directory name is not an app ID: use 1-32 of a-z, 0-9, and -",
        ));
    }
    let dir = format!("{}/{name}", location.root());
    let bytes = read_limited(filesystem, &dir, MANIFEST_FILE, MAX_MANIFEST_BYTES).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid(format!("{MANIFEST_FILE} is not JSON")))?;
    MANIFEST_SCHEMA
        .validate(&value)
        .map_err(|error| invalid(format!("{MANIFEST_FILE}: {error}")))?;
    let manifest: Manifest = serde_json::from_value(value)
        .map_err(|_| invalid(format!("{MANIFEST_FILE} does not match the app format")))?;
    if manifest.format != FORMAT_VERSION {
        return Err(invalid(format!(
            "{MANIFEST_FILE}: format {} is not supported",
            manifest.format
        )));
    }
    if manifest.id != name {
        return Err(invalid(format!(
            "{MANIFEST_FILE}: id \"{}\" does not match the directory name",
            manifest.id
        )));
    }
    for (index, permission) in manifest.permissions.iter().enumerate() {
        if manifest.permissions[..index].contains(permission) {
            return Err(invalid(format!(
                "{MANIFEST_FILE}: permission \"{permission}\" is listed twice"
            )));
        }
        if permission == SYSTEM_PACKAGE && location.kind() == AppKind::Installable {
            return Err(invalid(format!(
                "{MANIFEST_FILE}: permission \"{SYSTEM_PACKAGE}\" is only for system apps"
            )));
        }
    }
    let entry = manifest
        .entry
        .unwrap_or_else(|| String::from(DEFAULT_ENTRY));
    let mut files = alloc::vec![entry.as_str()];
    if let Some(view) = &manifest.view {
        files.push(&view.program);
        files.push(&view.styles);
        files.extend(view.fonts.iter().map(String::as_str));
    }
    for file in files {
        if !valid_file(file) {
            return Err(invalid(format!(
                "{MANIFEST_FILE}: \"{file}\" is not a file name in the container"
            )));
        }
        require_file(filesystem, &dir, file).await?;
    }
    Ok(Container {
        info: AppInfo {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            location,
            kind: location.kind(),
            has_view: manifest.view.is_some(),
            permissions: manifest
                .permissions
                .into_iter()
                .filter(|permission| permission != APP_PACKAGE)
                .collect(),
            icon: manifest.icon,
        },
        dir,
        entry,
        view: manifest.view,
    })
}

/// Container directory names at `location`; a missing root holds none.
async fn container_names(filesystem: &ScopedVfs, location: AppLocation) -> Vec<String> {
    let entries = match filesystem.read_dir(location.root()).await {
        Ok(entries) => entries,
        Err(FsError::NotFound | FsError::NotMounted) => return Vec::new(),
        Err(error) => {
            log::warn!("app: cannot list {}: {error:?}", location.root());
            return Vec::new();
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.metadata().is_dir() && !entry.file_name().starts_with('.'))
        .map(|entry| String::from(entry.file_name()))
        .collect();
    names.sort_unstable();
    names
}

/// Every container in every location.
pub(crate) async fn list(filesystem: &ScopedVfs) -> AppList {
    let mut apps = BTreeMap::new();
    let mut installable = Vec::new();
    let mut invalid = Vec::new();
    for location in [
        AppLocation::Bundled,
        AppLocation::Installed,
        AppLocation::Workspace,
    ] {
        for name in container_names(filesystem, location).await {
            let installed: Option<&AppInfo> = apps.get(&name);
            if installed.is_some_and(|app| app.kind == AppKind::System) {
                // A system app's ID is never installed over, so a container
                // reusing it elsewhere is reported rather than offered.
                if location != AppLocation::Bundled {
                    invalid.push(InvalidApp {
                        location,
                        dir: name,
                        problem: String::from("the ID belongs to a system app"),
                    });
                }
                continue;
            }
            match read_container(filesystem, location, &name).await {
                Ok(container) if location == AppLocation::Workspace => {
                    if installed.is_none_or(|app| app.version != container.info.version) {
                        installable.push(container.info);
                    }
                }
                Ok(container) => {
                    apps.insert(name, container.info);
                }
                Err(Problem::Invalid(problem)) => invalid.push(InvalidApp {
                    location,
                    dir: name,
                    problem,
                }),
                Err(Problem::Storage) => {
                    log::warn!("app: cannot read {}/{name}", location.root());
                }
            }
        }
    }
    AppList {
        apps: apps.into_values().collect(),
        installable,
        invalid,
    }
}

/// The container of launchable app `id`: its system app, or else its
/// installed app.
async fn find(filesystem: &ScopedVfs, id: &str) -> Result<Container, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    for location in AppLocation::LAUNCHABLE {
        let manifest = format!("{}/{id}/{MANIFEST_FILE}", location.root());
        if filesystem.exists(&manifest).await.unwrap_or(false) {
            return Ok(read_container(filesystem, location, id).await?);
        }
    }
    Err(AppError::NotFound)
}

/// The launchable app `id`.
pub(crate) async fn info(filesystem: &ScopedVfs, id: &str) -> Result<AppInfo, AppError> {
    Ok(find(filesystem, id).await?.info)
}

/// The Workspace container offered as app `id`.
pub(crate) async fn offered(filesystem: &ScopedVfs, id: &str) -> Result<Container, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    let manifest = format!("{WORKSPACE_APPS}/{id}/{MANIFEST_FILE}");
    if !filesystem.exists(&manifest).await.unwrap_or(false) {
        return Err(AppError::NotFound);
    }
    Ok(read_container(filesystem, AppLocation::Workspace, id).await?)
}

/// Reads everything needed to launch app `id`.
pub(crate) async fn load(filesystem: &ScopedVfs, id: &str) -> Result<Bundle, AppError> {
    let container = find(filesystem, id).await?;
    let source = read_limited(
        filesystem,
        &container.dir,
        &container.entry,
        MAX_SOURCE_BYTES,
    )
    .await?;
    let source = String::from_utf8(source).map_err(|_| AppError::InvalidBundle)?;
    let view = match &container.view {
        Some(files) => Some(load_view(filesystem, &container.dir, files).await?),
        None => None,
    };
    Ok(Bundle {
        info: container.info,
        source,
        view,
    })
}

async fn load_view(
    filesystem: &ScopedVfs,
    dir: &str,
    files: &ViewFiles,
) -> Result<UiAssets, Problem> {
    let mut budget = MAX_VIEW_BYTES;
    let mut take = async |name: &str| -> Result<Vec<u8>, Problem> {
        let bytes = read_limited(filesystem, dir, name, budget).await?;
        budget = budget
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| invalid("the view is larger than its budget"))?;
        Ok(bytes)
    };
    let view = take(&files.program).await?;
    let styles = take(&files.styles).await?;
    let mut fonts = Vec::with_capacity(files.fonts.len());
    for font in &files.fonts {
        fonts.push(take(font).await?);
    }
    Ok(UiAssets {
        view,
        styles,
        fonts,
    })
}
