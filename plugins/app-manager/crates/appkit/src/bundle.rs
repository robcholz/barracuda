//! App containers: one directory per app, or in the Workspace one `.bapp`
//! package, in the format `docs/app-format.md` defines and
//! `resources/app-manifest.json` checks.
//!
//! Where a container lives decides what kind of app it is: builtin apps'
//! containers, each in the Plugin that ships it, are system apps, and
//! installed ones are installable apps. Workspace containers are not
//! launchable; they are what an install copies into [`INSTALLED_APPS`].

use crate::AppError;
use crate::bapp::{PACKAGE_EXTENSION, Package, PackageError, Seal};
use crate::device::{self, API_LEVEL, Device, Packages, ViewShape};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    vec::Vec,
};
use barracuda_uikit::UiAssets;
use barracuda_vfs::{FsError, ScopedVfs};
use barracuda_vm_plugin::AppApi;
use core::cell::RefCell;
use json_validator::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Where a builtin app's container is, in the filesystem of the Plugin that
/// ships it.
pub const BUILTIN_APP: &str = "/resources/app";
/// Installable apps the person installed, inside this Plugin's private media.
pub const INSTALLED_APPS: &str = "/media/apps";
/// Containers that Agents, the portal, and other Plugins offer for
/// installation, in the shared Workspace.
pub const WORKSPACE_APPS: &str = "/workspace/media/apps";
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
    const LAUNCHABLE: [AppLocation; 2] = [AppLocation::Builtin, AppLocation::Installed];

    /// The kind of app a container here is.
    #[must_use]
    pub const fn kind(self) -> AppKind {
        match self {
            AppLocation::Builtin => AppKind::System,
            AppLocation::Installed | AppLocation::Workspace => AppKind::Installable,
        }
    }

    /// The directory holding this location's containers; for a builtin app,
    /// its container in its Plugin's filesystem.
    #[must_use]
    pub const fn root(self) -> &'static str {
        match self {
            AppLocation::Builtin => BUILTIN_APP,
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
    /// The app API level the app is written for.
    pub api: u32,
    /// The app API capabilities the app cannot run without.
    pub requires: Vec<String>,
    /// The app API capabilities the app uses when the device has them.
    pub uses: Vec<String>,
    /// The Lua packages outside the app API the app asks for: device-specific
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
    /// Why the app cannot run on this device, in the words the install
    /// screen shows; only for installable apps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incompatible: Option<String>,
    /// SHA-256 of the offered files, in hex: what an install asked for is
    /// pinned to. Only on an install request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The short name of the key that signed an offered package, when it
    /// is signed. Only on an install request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
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

/// Where a container's files are.
pub(crate) enum Source {
    /// Files in directory `dir`.
    Dir { filesystem: ScopedVfs, dir: String },
    /// Files in a package.
    Package {
        filesystem: ScopedVfs,
        package: Package,
    },
}

impl Source {
    /// Reads file `name`, refusing one larger than `limit` bytes.
    pub(crate) async fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, Problem> {
        match self {
            Self::Dir { filesystem, dir } => read_limited(filesystem, dir, name, limit).await,
            Self::Package {
                filesystem,
                package,
            } => Ok(package.read(filesystem, name, limit).await?),
        }
    }

    async fn require(&self, name: &str) -> Result<(), Problem> {
        match self {
            Self::Dir { filesystem, dir } => require_file(filesystem, dir, name).await,
            Self::Package { package, .. } if package.has(name) => Ok(()),
            Self::Package { .. } => Err(invalid(format!("{name} is missing"))),
        }
    }
}

impl From<PackageError> for Problem {
    fn from(error: PackageError) -> Self {
        match error {
            PackageError::Storage => Problem::Storage,
            PackageError::Invalid(message) => Problem::Invalid(message),
        }
    }
}

/// A container checked against the app format.
pub(crate) struct Container {
    pub(crate) info: AppInfo,
    pub(crate) source: Source,
    entry: String,
    view: Option<ViewFiles>,
}

impl Container {
    /// What the container's files hash to, and who signed them: a
    /// package's signature, checked; a directory's files, each preceded by
    /// its name and length, are unsigned.
    pub(crate) async fn seal(&self) -> Result<Seal, AppError> {
        if let Source::Package {
            filesystem,
            package,
        } = &self.source
        {
            return Ok(package.seal(filesystem).await.map_err(Problem::from)?);
        }
        let mut files = self.files();
        files.sort_unstable();
        let mut hasher = Sha256::new();
        for name in files {
            let bytes = self.source.read(name, MAX_VIEW_BYTES).await?;
            hasher.update(name.as_bytes());
            hasher.update([0]);
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        Ok(Seal {
            digest: hasher.finalize().into(),
            signer: None,
        })
    }

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
        return Err(invalid(
            "the directory name is not an app ID: use 1-32 of a-z, 0-9, and -",
        ));
    }
    let source = shelf.source(location, name).await?;
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
            digest: None,
            signer: None,
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

/// Checks the packages a manifest asks for against the app API: capabilities
/// in `requires` and `uses`, everything else in `permissions`, each once, and
/// `boards` for packages that talk to a Board's hardware.
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
    for (list, names) in lists {
        for name in names {
            if !seen.insert(name.as_str()) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is listed twice"
                )));
            }
            if device::is_core(name) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is a core package, which every app has"
                )));
            }
            if location.kind() == AppKind::Installable && shelf.builtins.reserves(name) {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" is only for builtin apps"
                )));
            }
            let capability = device::is_capability(name, API_LEVEL);
            match list {
                "permissions" if capability => {
                    return Err(invalid(format!(
                        "{MANIFEST_FILE}: \"{name}\" is an app API capability: list it in requires or uses"
                    )));
                }
                // Capabilities of a later level are checked by a later system.
                "requires" | "uses"
                    if manifest.api <= API_LEVEL && !device::is_capability(name, manifest.api) =>
                {
                    return Err(invalid(format!(
                        "{MANIFEST_FILE}: \"{name}\" is not a capability of app API {}",
                        manifest.api
                    )));
                }
                _ => {}
            }
            let hardware = matches!(
                shelf.packages.get(name.as_str()),
                Some(Some(AppApi::DeviceSpecific { .. }))
            );
            if hardware && manifest.boards.is_empty() {
                return Err(invalid(format!(
                    "{MANIFEST_FILE}: \"{name}\" talks to a Board's hardware: list the boards the app is for"
                )));
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

/// Where containers are, this Plugin's filesystem and the builtin apps', and
/// the device installable apps are checked against.
pub(crate) struct Shelf<'a> {
    pub(crate) filesystem: &'a ScopedVfs,
    pub(crate) builtins: &'a Builtins,
    pub(crate) device: &'a Device,
    /// The registered Lua packages.
    pub(crate) packages: Packages,
}

impl Shelf<'_> {
    /// The filesystem and directory of container `name` at `location`.
    fn place(&self, location: AppLocation, name: &str) -> Option<(ScopedVfs, String)> {
        match location {
            AppLocation::Builtin => self
                .builtins
                .apps
                .borrow()
                .get(name)
                .map(|filesystem| (filesystem.clone(), String::from(BUILTIN_APP))),
            AppLocation::Installed | AppLocation::Workspace => Some((
                self.filesystem.clone(),
                format!("{}/{name}", location.root()),
            )),
        }
    }

    /// Where container `name`'s files are at `location`: its directory, or
    /// in the Workspace its package when no directory has the name.
    async fn source(&self, location: AppLocation, name: &str) -> Result<Source, Problem> {
        let (filesystem, dir) = self.place(location, name).ok_or(Problem::Storage)?;
        if location != AppLocation::Workspace {
            return Ok(Source::Dir { filesystem, dir });
        }
        let package = format!("{dir}{PACKAGE_EXTENSION}");
        let is_dir = filesystem
            .metadata(&dir)
            .await
            .is_ok_and(|metadata| metadata.is_dir());
        let is_package = filesystem.exists(&package).await.unwrap_or(false);
        match (is_dir, is_package) {
            (true, true) => Err(invalid("offered both as a directory and as a package")),
            (false, true) => Ok(Source::Package {
                package: Package::open(&filesystem, &package).await?,
                filesystem,
            }),
            _ => Ok(Source::Dir { filesystem, dir }),
        }
    }
}

/// Container directory names at `location`, or builtin apps' IDs; a missing
/// root holds none.
async fn container_names(shelf: &Shelf<'_>, location: AppLocation) -> Vec<String> {
    if location == AppLocation::Builtin {
        return shelf.builtins.ids();
    }
    let filesystem = shelf.filesystem;
    let entries = match filesystem.read_dir(location.root()).await {
        Ok(entries) => entries,
        Err(FsError::NotFound | FsError::NotMounted) => return Vec::new(),
        Err(error) => {
            log::warn!("app: cannot list {}: {error:?}", location.root());
            return Vec::new();
        }
    };
    let packages = location == AppLocation::Workspace;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().starts_with('.'))
        .filter_map(|entry| {
            let name = entry.file_name();
            if entry.metadata().is_dir() {
                Some(String::from(name))
            } else if packages {
                name.strip_suffix(PACKAGE_EXTENSION).map(String::from)
            } else {
                None
            }
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Every container in every location.
pub(crate) async fn list(shelf: &Shelf<'_>) -> AppList {
    let mut apps = BTreeMap::new();
    let mut installable = Vec::new();
    let mut invalid = Vec::new();
    for location in [
        AppLocation::Builtin,
        AppLocation::Installed,
        AppLocation::Workspace,
    ] {
        for name in container_names(shelf, location).await {
            let installed: Option<&AppInfo> = apps.get(&name);
            if installed.is_some_and(|app| app.kind == AppKind::System) {
                // A system app's ID is never installed over, so a container
                // reusing it elsewhere is reported rather than offered.
                if location != AppLocation::Builtin {
                    invalid.push(InvalidApp {
                        location,
                        dir: name,
                        problem: String::from("the ID belongs to a system app"),
                    });
                }
                continue;
            }
            match read_container(shelf, location, &name).await {
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
async fn find(shelf: &Shelf<'_>, id: &str) -> Result<Container, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    for location in AppLocation::LAUNCHABLE {
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

/// The launchable app `id`.
pub(crate) async fn info(shelf: &Shelf<'_>, id: &str) -> Result<AppInfo, AppError> {
    Ok(find(shelf, id).await?.info)
}

/// The Workspace container offered as app `id`.
pub(crate) async fn offered(shelf: &Shelf<'_>, id: &str) -> Result<Container, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    let manifest = format!("{WORKSPACE_APPS}/{id}/{MANIFEST_FILE}");
    let package = format!("{WORKSPACE_APPS}/{id}{PACKAGE_EXTENSION}");
    let filesystem = shelf.filesystem;
    if !filesystem.exists(&manifest).await.unwrap_or(false)
        && !filesystem.exists(&package).await.unwrap_or(false)
    {
        return Err(AppError::NotFound);
    }
    Ok(read_container(shelf, AppLocation::Workspace, id).await?)
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
