//! App containers: one directory per app, in the format
//! `docs/plugins/app-manager/app-format.mdx` defines and
//! `resources/app-manifest.json` checks.
//!
//! Where a container lives decides what kind of app it is: builtin apps'
//! containers, each in the Plugin that ships it, are system apps, and the
//! `<id>.bapp` directories under [`WORKSPACE_APPS`] are installable apps.
//! An app is installed by putting its directory there and uninstalled by
//! removing it.

use crate::AppError;
use crate::device::{self, Device, Packages, ViewShape};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    vec::Vec,
};
use barracuda_uikit::UiAssets;
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_vm_plugin::PackageInterface;
use core::cell::RefCell;
use json_validator::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a builtin app's container is, in the filesystem of the Plugin that
/// ships it.
pub const BUILTIN_APP: &str = "/resources/app";
/// Installable apps, one `<id>.bapp` directory each, in the shared Workspace,
/// where the portal, Agents, and other Plugins write them.
pub const WORKSPACE_APPS: &str = "/workspace/media/apps";
/// What an installable app's directory name ends in, after its ID.
pub const APP_EXTENSION: &str = ".bapp";
/// The most Lua packages each of a manifest's `requires`, `uses`, and
/// `permissions` may list.
pub const MAX_PERMISSIONS: usize = 16;
/// The package every app program uses, granted without being asked for.
pub const APP_PACKAGE: &str = "app";

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
    /// Shipped by a builtin app Plugin, at [`BUILTIN_APP`] in its filesystem.
    Builtin,
    /// A `<id>.bapp` directory in the Workspace under [`WORKSPACE_APPS`].
    Workspace,
}

/// What an app may do, decided by where its container lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    /// Shipped in the image: may require every Lua package, cannot be
    /// uninstalled, and no installable app may take its ID.
    System,
    /// In the Workspace: requires only the ambient packages and the ones its
    /// manifest names, and can be uninstalled.
    Installable,
}

impl AppLocation {
    /// The locations, in the order an app ID is looked up: a system app's ID
    /// is never taken by an installable app.
    const ALL: [AppLocation; 2] = [AppLocation::Builtin, AppLocation::Workspace];

    /// The kind of app a container here is.
    #[must_use]
    pub const fn kind(self) -> AppKind {
        match self {
            AppLocation::Builtin => AppKind::System,
            AppLocation::Workspace => AppKind::Installable,
        }
    }

    /// The directory holding this location's containers; for a builtin app,
    /// its container in its Plugin's filesystem.
    #[must_use]
    pub const fn root(self) -> &'static str {
        match self {
            AppLocation::Builtin => BUILTIN_APP,
            AppLocation::Workspace => WORKSPACE_APPS,
        }
    }
}

/// One app container that passed the app format.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AppInfo {
    /// Stable ID: a builtin app's registered ID, an installable app's
    /// directory name before [`APP_EXTENSION`].
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
    /// The version of the `app` package the app is written for.
    pub api: u32,
    /// The stable packages the app cannot run without, each with an optional
    /// `@version`, the least it needs.
    pub requires: Vec<String>,
    /// The stable packages the app uses when the device has them, written as
    /// in `requires`.
    pub uses: Vec<String>,
    /// The other Lua packages the app asks for: Board-specific
    /// ones and others. Only an installable app is limited to its packages.
    pub permissions: Vec<String>,
    /// The Boards the app is for; empty when it is for any.
    pub boards: Vec<String>,
    /// The size its view was built for, if it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_size: Option<[u32; 2]>,
    /// The shape its view was built for, if it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_shape: Option<ViewShape>,
    /// The name of the system icon the app is shown with, if it picked one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Why the app cannot run on this device, in the person's words; only
    /// for installable apps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incompatible: Option<String>,
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

/// Every container, split into apps and broken ones.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct AppList {
    /// System and installable apps, sorted by ID; an installable app that
    /// cannot run on this device says why in `incompatible`.
    pub apps: Vec<AppInfo>,
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
    #[serde(default = "first_level")]
    api: u32,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    uses: Vec<String>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    boards: Vec<String>,
    #[serde(default)]
    icon: Option<String>,
}

const fn first_level() -> u32 {
    1
}

#[derive(Deserialize)]
struct ViewFiles {
    program: String,
    styles: String,
    #[serde(default)]
    fonts: Vec<String>,
    #[serde(default)]
    size: Option<[u32; 2]>,
    #[serde(default)]
    shape: Option<ViewShape>,
}

/// Where a container's files are: directory `dir` in `filesystem`.
pub(crate) struct Source {
    filesystem: ScopedVfs,
    dir: String,
}

impl Source {
    /// Reads file `name`, refusing one larger than `limit` bytes.
    async fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, Problem> {
        read_limited(&self.filesystem, &self.dir, name, limit).await
    }

    async fn require(&self, name: &str) -> Result<(), Problem> {
        require_file(&self.filesystem, &self.dir, name).await
    }
}

/// A container checked against the app format.
pub(crate) struct Container {
    pub(crate) info: AppInfo,
    source: Source,
    entry: String,
    view: Option<ViewFiles>,
}

pub(crate) struct Bundle {
    pub(crate) info: AppInfo,
    pub(crate) source: String,
    pub(crate) view: Option<UiAssets>,
}

pub(crate) enum Problem {
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
    shelf: &Shelf<'_>,
    location: AppLocation,
    name: &str,
) -> Result<Container, Problem> {
    if !valid_id(name) {
        return Err(invalid(format!(
            "the directory name is not an app ID followed by {APP_EXTENSION}: use 1-32 of a-z, 0-9, and -"
        )));
    }
    let (filesystem, dir) = shelf.place(location, name).ok_or(Problem::Storage)?;
    let source = Source { filesystem, dir };
    let bytes = source.read(MANIFEST_FILE, MAX_MANIFEST_BYTES).await?;
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
            "{MANIFEST_FILE}: id \"{}\" does not match the container's name",
            manifest.id
        )));
    }
    check_packages(&manifest, location, shelf)?;
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
        source.require(file).await?;
    }
    let mut container = Container {
        info: AppInfo {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            location,
            kind: location.kind(),
            has_view: manifest.view.is_some(),
            api: manifest.api,
            requires: manifest.requires,
            uses: manifest.uses,
            permissions: manifest.permissions,
            boards: manifest.boards,
            view_size: manifest.view.as_ref().and_then(|view| view.size),
            view_shape: manifest.view.as_ref().and_then(|view| view.shape),
            icon: manifest.icon,
            incompatible: None,
        },
        source,
        entry,
        view: manifest.view,
    };
    if location.kind() == AppKind::Installable {
        container.info.incompatible =
            device::incompatibility(&container.info, shelf.device, &shelf.packages);
    }
    Ok(container)
}

/// Checks the packages a manifest asks for against what the registered
/// packages promise: stable ones in `requires` and `uses`, with an optional
/// `@version`, everything else in `permissions`, each once, and `boards` for
/// packages that reach a Board's hardware. A package this device lacks only
/// makes the app incompatible here.
fn check_packages(
    manifest: &Manifest,
    location: AppLocation,
    shelf: &Shelf<'_>,
) -> Result<(), Problem> {
    let lists = [
        ("requires", &manifest.requires),
        ("uses", &manifest.uses),
        ("permissions", &manifest.permissions),
    ];
    let mut seen = BTreeSet::new();
    for (list, entries) in lists {
        for entry in entries {
            let (name, version) = if list == "permissions" {
                (entry.as_str(), 1)
            } else {
                device::requirement(entry)
            };
            let bad_name = name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte));
            if bad_name || version == 0 || (list == "permissions" && entry.contains('@')) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{entry}\" is not a package name, with a version after @ only in requires and uses"
                )));
            }
            if !seen.insert(name) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is listed twice"
                )));
            }
            if device::is_ambient(&shelf.packages, name) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is a package every app has"
                )));
            }
            if location.kind() == AppKind::Installable && shelf.builtins.reserves(name) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is only for builtin apps"
                )));
            }
            match (list, shelf.packages.get(name)) {
                ("permissions", Some(Some(PackageInterface::Stable { .. }))) => {
                    return Err(invalid(format!(
                        "{MANIFEST_FILE}: \"{name}\" is a stable package: list it in requires or uses"
                    )));
                }
                ("requires" | "uses", Some(Some(PackageInterface::BoardSpecific { .. }))) => {
                    return Err(invalid(format!(
                        "{MANIFEST_FILE}: \"{name}\" is not a stable package: list it in permissions"
                    )));
                }
                (_, Some(Some(PackageInterface::BoardSpecific { .. })))
                    if manifest.boards.is_empty() =>
                {
                    return Err(invalid(format!(
                        "{MANIFEST_FILE}: \"{name}\" talks to a Board's hardware: list the boards the app is for"
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Builtin apps, each registered by the Plugin that ships it, and the Lua
/// packages they reserve.
#[derive(Default)]
pub(crate) struct Builtins {
    apps: RefCell<BTreeMap<String, ScopedVfs>>,
    reserved: RefCell<BTreeSet<String>>,
    /// What each app opens for, by app ID.
    opens: RefCell<BTreeMap<String, &'static [&'static str]>>,
}

impl Builtins {
    /// Adds builtin app `id`, whose container is at [`BUILTIN_APP`] in
    /// `filesystem`; returns `false` when the ID is taken.
    pub(crate) fn add(
        &self,
        id: &str,
        filesystem: ScopedVfs,
        opens: &'static [&'static str],
    ) -> bool {
        let mut apps = self.apps.borrow_mut();
        if apps.contains_key(id) {
            return false;
        }
        apps.insert(String::from(id), filesystem);
        self.opens.borrow_mut().insert(String::from(id), opens);
        true
    }

    /// The builtin app that opens for `what`, if one does.
    pub(crate) fn opening(&self, what: &str) -> Option<String> {
        self.opens
            .borrow()
            .iter()
            .find(|(_, opens)| opens.contains(&what))
            .map(|(id, _)| id.clone())
    }

    /// Keeps Lua package `name` for builtin apps.
    pub(crate) fn reserve(&self, name: &str) {
        self.reserved.borrow_mut().insert(String::from(name));
    }

    /// Whether only builtin apps may require package `name`.
    pub(crate) fn reserves(&self, name: &str) -> bool {
        self.reserved.borrow().contains(name)
    }

    fn ids(&self) -> Vec<String> {
        self.apps.borrow().keys().cloned().collect()
    }
}

/// Where containers are, the Workspace in this Plugin's filesystem and the
/// builtin apps', and the device installable apps are checked against.
pub(crate) struct Shelf<'a> {
    pub(crate) filesystem: &'a ScopedVfs,
    pub(crate) builtins: &'a Builtins,
    pub(crate) device: &'a Device,
    /// The registered Lua packages.
    pub(crate) packages: Packages,
}

impl Shelf<'_> {
    /// The filesystem and directory of app `id` at `location`.
    fn place(&self, location: AppLocation, id: &str) -> Option<(ScopedVfs, String)> {
        match location {
            AppLocation::Builtin => self
                .builtins
                .apps
                .borrow()
                .get(id)
                .map(|filesystem| (filesystem.clone(), String::from(BUILTIN_APP))),
            AppLocation::Workspace => Some((
                self.filesystem.clone(),
                format!("{WORKSPACE_APPS}/{id}{APP_EXTENSION}"),
            )),
        }
    }
}

/// What is in the Workspace's apps directory: the IDs of its `<id>.bapp`
/// directories, and the other directories, which hold no app; a missing
/// directory holds none. Files and names starting with `.` are skipped.
async fn workspace_entries(filesystem: &ScopedVfs) -> (Vec<String>, Vec<String>) {
    let entries = match filesystem.read_dir(WORKSPACE_APPS).await {
        Ok(entries) => entries,
        Err(FsError::NotFound | FsError::NotMounted) => return (Vec::new(), Vec::new()),
        Err(error) => {
            log::warn!("app: cannot list {WORKSPACE_APPS}: {error:?}");
            return (Vec::new(), Vec::new());
        }
    };
    let (mut ids, mut others) = (Vec::new(), Vec::new());
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if name.starts_with('.') || !entry.metadata().is_dir() {
            continue;
        }
        match name.strip_suffix(APP_EXTENSION) {
            Some(id) => ids.push(String::from(id)),
            None => others.push(String::from(name)),
        }
    }
    ids.sort_unstable();
    others.sort_unstable();
    (ids, others)
}

/// Every container in every location.
pub(crate) async fn list(shelf: &Shelf<'_>) -> AppList {
    let mut apps = BTreeMap::new();
    let mut invalid = Vec::new();
    let builtins = shelf.builtins.ids();
    for id in builtins.iter().cloned() {
        match read_container(shelf, AppLocation::Builtin, &id).await {
            Ok(container) => {
                apps.insert(id, container.info);
            }
            Err(Problem::Invalid(problem)) => invalid.push(InvalidApp {
                location: AppLocation::Builtin,
                dir: id,
                problem,
            }),
            Err(Problem::Storage) => log::warn!("app: cannot read builtin app {id}"),
        }
    }
    let (ids, others) = workspace_entries(shelf.filesystem).await;
    for id in ids {
        let dir = format!("{id}{APP_EXTENSION}");
        if builtins.contains(&id) {
            // A system app's ID is never taken, so a directory reusing it is
            // reported rather than shown.
            invalid.push(InvalidApp {
                location: AppLocation::Workspace,
                dir,
                problem: String::from("the ID belongs to a system app"),
            });
            continue;
        }
        match read_container(shelf, AppLocation::Workspace, &id).await {
            Ok(container) => {
                apps.insert(id, container.info);
            }
            Err(Problem::Invalid(problem)) => invalid.push(InvalidApp {
                location: AppLocation::Workspace,
                dir,
                problem,
            }),
            Err(Problem::Storage) => log::warn!("app: cannot read {WORKSPACE_APPS}/{dir}"),
        }
    }
    for dir in others {
        invalid.push(InvalidApp {
            location: AppLocation::Workspace,
            problem: format!("an app's directory is named after its ID, as {dir}{APP_EXTENSION}"),
            dir,
        });
    }
    AppList {
        apps: apps.into_values().collect(),
        invalid,
    }
}

/// The container of app `id`: its system app, or else its installable app.
async fn find(shelf: &Shelf<'_>, id: &str) -> Result<Container, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    for location in AppLocation::ALL {
        let Some((filesystem, dir)) = shelf.place(location, id) else {
            continue;
        };
        let manifest = format!("{dir}/{MANIFEST_FILE}");
        if filesystem.exists(&manifest).await.unwrap_or(false) {
            return Ok(read_container(shelf, location, id).await?);
        }
    }
    Err(AppError::NotFound)
}

/// App `id`.
pub(crate) async fn info(shelf: &Shelf<'_>, id: &str) -> Result<AppInfo, AppError> {
    Ok(find(shelf, id).await?.info)
}

/// Reads everything needed to launch app `id`.
pub(crate) async fn load(shelf: &Shelf<'_>, id: &str) -> Result<Bundle, AppError> {
    let container = find(shelf, id).await?;
    let source = container
        .source
        .read(&container.entry, MAX_SOURCE_BYTES)
        .await?;
    let source = String::from_utf8(source).map_err(|_| AppError::InvalidBundle)?;
    let view = match &container.view {
        Some(files) => Some(load_view(&container.source, files).await?),
        None => None,
    };
    Ok(Bundle {
        info: container.info,
        source,
        view,
    })
}

async fn load_view(source: &Source, files: &ViewFiles) -> Result<UiAssets, Problem> {
    let mut budget = MAX_VIEW_BYTES;
    let mut take = async |name: &str| -> Result<Vec<u8>, Problem> {
        let bytes = source.read(name, budget).await?;
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
