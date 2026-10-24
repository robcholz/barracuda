//! Which Plugins a Board enables, and which devices their peripheral slots
//! receive.
//!
//! A Board names a profile (a named Plugin set under `boards/profiles`) and
//! overrides single Plugins in its `plugins.<id>` section. Enabled Plugins
//! bring their dependencies. A Plugin declares peripheral slots in
//! `plugin.toml`; the Board supplies named devices and wires slots only when
//! the defaults cannot decide. Everything here is resolved at build time.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

use barracuda_board_config::{
    parse_profile, BoardDefinition, DEFAULT_PROFILE, PROFILES_PATH, SELECTED_BOARD_PATH,
};
use barracuda_plugin_manifest::{parse, PeripheralClaim, PeripheralFill, PluginManifest};

use crate::TaskError;

/// File listing Plugins a developer disables locally, on top of the Board.
pub const LOCAL_DISABLED_PATH: &str = ".barracuda/disabled-plugins";

/// Peripheral kinds that System owns and Plugins cannot claim.
const SYSTEM_PERIPHERALS: &[&str] = &["removable-storage"];

/// One discovered Plugin: its directory under `plugins/` and its manifest.
#[derive(Debug)]
pub struct PluginSource {
    /// Directory name under `plugins/`.
    pub directory: String,
    /// Parsed `plugin.toml`.
    pub manifest: PluginManifest,
}

/// One device a slot receives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedDevice {
    /// The Board's name for the device.
    pub name: String,
    /// The device's position among the Board's devices of its kind, in name
    /// order: the instance index the generated Board HAL takes it by.
    pub index: u8,
}

/// What one slot receives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Filled {
    /// A `"primary"` or `"any"` slot: one device, or none when optional.
    One(Option<ClaimedDevice>),
    /// A `"*"` slot: every device nothing else took.
    Many(Vec<ClaimedDevice>),
}

/// The resolved Plugin set and peripheral claims for one Board.
#[derive(Debug, Default)]
pub struct Selection {
    enabled: Vec<String>,
    notes: Vec<String>,
    claims: Claims,
}

impl Selection {
    /// Enabled Plugin identities, in directory order.
    #[must_use]
    pub fn enabled(&self) -> &[String] {
        &self.enabled
    }

    /// Whether the Plugin with this identity is enabled.
    #[must_use]
    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|enabled| enabled == id)
    }

    /// Explanations of Plugins disabled by the local override's cascade.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The devices one Plugin's slots receive, keyed by `(kind, slot)`.
    /// Empty for Plugins that are not enabled or claim nothing.
    pub fn claims(&self, id: &str) -> impl Iterator<Item = (&str, &str, &Filled)> {
        self.claims
            .get(id)
            .into_iter()
            .flatten()
            .map(|((kind, slot), filled)| (kind.as_str(), slot.as_str(), filled))
    }
}

fn io_error(path: &Path, error: &io::Error) -> TaskError {
    TaskError::Task(format!("{}: {error}", path.display()))
}

/// Discovers every Plugin with a crate under `plugins/`, in directory order.
///
/// # Errors
/// Reports unreadable directories and invalid manifests.
pub fn discover(workspace: &Path) -> Result<Vec<PluginSource>, TaskError> {
    let root = workspace.join("plugins");
    let mut plugins = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(plugins),
        Err(error) => return Err(io_error(&root, &error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error(&root, &error))?;
        let path = entry.path();
        if !path.join("crates/plugin/Cargo.toml").is_file() {
            continue;
        }
        let Ok(directory) = entry.file_name().into_string() else {
            continue;
        };
        let manifest_path = path.join("plugin.toml");
        let text =
            fs::read_to_string(&manifest_path).map_err(|error| io_error(&manifest_path, &error))?;
        let manifest = parse(&text)
            .map_err(|error| TaskError::Task(format!("{}: {error}", manifest_path.display())))?;
        plugins.push(PluginSource {
            directory,
            manifest,
        });
    }
    plugins.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(plugins)
}

/// Reads and parses one Board's `board.yml`.
///
/// # Errors
/// Reports a missing or invalid Board.
pub fn load_board(workspace: &Path, name: &str) -> Result<BoardDefinition, TaskError> {
    let path = workspace
        .join("boards/configs")
        .join(name)
        .join("board.yml");
    let text = fs::read_to_string(&path).map_err(|error| io_error(&path, &error))?;
    barracuda_board_config::parse(&text)
        .map_err(|error| TaskError::Task(format!("{}: {error}", path.display())))
}

/// Reads the selected Board, if one is selected.
///
/// # Errors
/// Reports an unreadable selection or an invalid Board.
pub fn selected_board(workspace: &Path) -> Result<Option<BoardDefinition>, TaskError> {
    let name = barracuda_board_config::read_selected_board(workspace)
        .map_err(|error| TaskError::Task(error.to_string()))?;
    name.map(|name| load_board(workspace, &name)).transpose()
}

/// Reads the local override: Plugin identities or directories disabled on
/// this machine only.
///
/// # Errors
/// Reports an unreadable override file.
pub fn local_disabled(workspace: &Path) -> Result<Vec<String>, TaskError> {
    let path = workspace.join(LOCAL_DISABLED_PATH);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(io_error(&path, &error)),
    }
}

/// Paths whose contents decide a selection, for build-script invalidation.
#[must_use]
pub fn inputs(workspace: &Path, plugins: &[PluginSource]) -> Vec<std::path::PathBuf> {
    let mut paths = vec![
        workspace.join(SELECTED_BOARD_PATH),
        workspace.join(LOCAL_DISABLED_PATH),
        workspace.join(PROFILES_PATH),
        workspace.join("peripherals/impl"),
    ];
    if let Ok(Some(name)) = barracuda_board_config::read_selected_board(workspace) {
        paths.push(
            workspace
                .join("boards/configs")
                .join(name)
                .join("board.yml"),
        );
    }
    paths.extend(plugins.iter().map(|plugin| {
        workspace
            .join("plugins")
            .join(&plugin.directory)
            .join("plugin.toml")
    }));
    paths
}

/// Computes which Plugins `board` enables and resolves their peripheral
/// claims, applying the local override last.
///
/// # Errors
/// Reports unknown profiles or Plugins, a disabled dependency, and every
/// peripheral claim that cannot be resolved.
pub fn select(
    workspace: &Path,
    board: &BoardDefinition,
    plugins: &[PluginSource],
    locally_disabled: &[String],
) -> Result<Selection, TaskError> {
    if plugins.is_empty() {
        return Ok(Selection::default());
    }
    let board_name = board.name();
    let id_of = |name: &str| {
        plugins
            .iter()
            .find(|plugin| plugin.manifest.id() == name || plugin.directory == name)
            .map(|plugin| plugin.manifest.id().to_owned())
    };
    let known = |id: &str, context: &dyn Fn() -> String| {
        id_of(id).ok_or_else(|| TaskError::Task(format!("{}: unknown Plugin `{id}`", context())))
    };

    // 1. The profile chain.
    let mut enabled = BTreeSet::new();
    for (profile, ids) in profile_chain(workspace, board.profile().unwrap_or(DEFAULT_PROFILE))? {
        for id in ids {
            enabled.insert(known(&id, &|| format!("profile `{profile}`"))?);
        }
    }

    // 2. The Board's overrides.
    let mut board_disabled = BTreeSet::new();
    for id in board.configured_plugins() {
        let context = || format!("Board `{board_name}` configures `plugins.{id}`");
        let id = known(id, &context)?;
        match board.plugin_enabled(&id) {
            Some(true) => {
                enabled.insert(id);
            }
            Some(false) => {
                enabled.remove(&id);
                board_disabled.insert(id);
            }
            None => {}
        }
    }

    // 3. Dependencies follow the Plugins that need them.
    let manifest = |id: &str| {
        plugins
            .iter()
            .map(|plugin| &plugin.manifest)
            .find(|manifest| manifest.id() == id)
    };
    let mut pending = enabled.iter().cloned().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        for dependency in manifest(&id)
            .map(PluginManifest::dependencies)
            .unwrap_or_default()
        {
            if board_disabled.contains(dependency) {
                return Err(TaskError::Task(format!(
                    "Board `{board_name}` enables `{id}`, which depends on `{dependency}`, but disables `{dependency}`"
                )));
            }
            if enabled.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }

    // 4. The local override, cascading to dependents.
    let mut notes = Vec::new();
    let mut removed = locally_disabled
        .iter()
        .filter_map(|name| id_of(name))
        .filter(|id| enabled.remove(id))
        .collect::<Vec<_>>();
    while let Some(id) = removed.pop() {
        let dependents = enabled
            .iter()
            .filter(|candidate| {
                manifest(candidate).is_some_and(|manifest| manifest.dependencies().contains(&id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for dependent in dependents {
            enabled.remove(&dependent);
            notes.push(format!(
                "`{dependent}` is disabled because `{id}` is disabled locally"
            ));
            removed.push(dependent);
        }
    }

    let enabled = plugins
        .iter()
        .map(|plugin| plugin.manifest.id().to_owned())
        .filter(|id| enabled.contains(id))
        .collect::<Vec<_>>();
    let claims = resolve_claims(workspace, board, plugins, &enabled)?;
    Ok(Selection {
        enabled,
        notes,
        claims,
    })
}

/// Loads a profile and every profile it extends, each once, the ones it
/// extends before it.
fn profile_chain(workspace: &Path, name: &str) -> Result<Vec<(String, Vec<String>)>, TaskError> {
    let mut chain = Vec::new();
    load_profile(workspace, name, &mut Vec::new(), &mut chain)?;
    Ok(chain)
}

fn load_profile(
    workspace: &Path,
    name: &str,
    path: &mut Vec<String>,
    chain: &mut Vec<(String, Vec<String>)>,
) -> Result<(), TaskError> {
    if path.iter().any(|seen| seen == name) {
        return Err(TaskError::Task(format!(
            "profile `{name}` extends itself through {}",
            path.iter()
                .map(|seen| format!("`{seen}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if chain.iter().any(|(loaded, _)| loaded == name) {
        return Ok(());
    }
    let file = workspace.join(PROFILES_PATH).join(format!("{name}.yml"));
    let text = fs::read_to_string(&file).map_err(|error| {
        TaskError::Task(format!(
            "unknown profile `{name}` ({}): {error}",
            file.display()
        ))
    })?;
    let profile = parse_profile(&text)
        .map_err(|error| TaskError::Task(format!("{}: {error}", file.display())))?;
    path.push(name.to_owned());
    for parent in profile.extends() {
        load_profile(workspace, parent, path, chain)?;
    }
    path.pop();
    chain.push((name.to_owned(), profile.enable().to_vec()));
    Ok(())
}

/// The Board's devices of each peripheral kind, in name order, with the
/// primary one of each kind.
struct Devices {
    by_kind: BTreeMap<String, Vec<String>>,
    primary: BTreeMap<String, String>,
}

impl Devices {
    fn load(workspace: &Path, board: &BoardDefinition) -> Result<Self, TaskError> {
        let catalog = barracuda_peripheral_config::load_catalog(workspace)
            .map_err(|error| TaskError::Task(error.to_string()))?;
        let mut by_kind = BTreeMap::<String, Vec<String>>::new();
        let mut marked = BTreeMap::<String, Vec<String>>::new();
        for (name, device) in board.peripheral_devices() {
            let kind = catalog
                .implementation(device.implementation())
                .ok_or_else(|| {
                    TaskError::Task(format!(
                        "Board `{}` device `{name}` uses unknown implementation `{}`",
                        board.name(),
                        device.implementation()
                    ))
                })?
                .peripheral()
                .to_owned();
            by_kind
                .entry(kind.clone())
                .or_default()
                .push(name.to_owned());
            if device.primary() {
                marked.entry(kind).or_default().push(name.to_owned());
            }
        }
        let mut primary = BTreeMap::new();
        for (kind, devices) in &by_kind {
            let chosen = match (devices.as_slice(), marked.get(kind).map(Vec::as_slice)) {
                ([only], _) => only.clone(),
                (_, Some([one])) => one.clone(),
                (_, marked) => {
                    return Err(TaskError::Task(format!(
                        "Board `{}` has {} `{kind}` devices ({}); mark exactly one `primary: true`{}",
                        board.name(),
                        devices.len(),
                        devices.join(", "),
                        marked.map_or(String::new(), |marked| format!(
                            ", not {}",
                            marked.join(" and ")
                        )),
                    )));
                }
            };
            primary.insert(kind.clone(), chosen);
        }
        Ok(Self { by_kind, primary })
    }

    fn device(&self, kind: &str, name: &str) -> Option<ClaimedDevice> {
        let index = self
            .by_kind
            .get(kind)?
            .iter()
            .position(|device| device == name)?;
        Some(ClaimedDevice {
            name: name.to_owned(),
            index: u8::try_from(index).ok()?,
        })
    }

    fn kind_of(&self, name: &str) -> Option<&str> {
        self.by_kind
            .iter()
            .find(|(_, devices)| devices.iter().any(|device| device == name))
            .map(|(kind, _)| kind.as_str())
    }
}

type Slot = (String, String, String);

/// Each Plugin's filled slots, keyed by `(kind, slot)`.
type Claims = BTreeMap<String, BTreeMap<(String, String), Filled>>;

fn suggestion(plugin: &str, kind: &str, slot: &str, candidates: &[String]) -> String {
    let candidates = if candidates.is_empty() {
        String::from("<device>")
    } else {
        candidates.join(" | ")
    };
    format!(
        "\n  wire it in board.yml:\n    plugins:\n      {plugin}:\n        peripherals:\n          {kind}.{slot}: {candidates}"
    )
}

fn resolve_claims(
    workspace: &Path,
    board: &BoardDefinition,
    plugins: &[PluginSource],
    enabled: &[String],
) -> Result<Claims, TaskError> {
    let board_name = board.name();
    let claiming = plugins
        .iter()
        .filter(|plugin| enabled.contains(&plugin.manifest.id().to_owned()))
        .filter(|plugin| !plugin.manifest.peripherals.is_empty())
        .collect::<Vec<_>>();
    let wired = board
        .configured_plugins()
        .any(|id| board.plugin_peripherals(id).next().is_some());
    if claiming.is_empty() && !wired {
        return Ok(BTreeMap::new());
    }
    let devices = Devices::load(workspace, board)?;

    let mut slots = Vec::<(Slot, PeripheralClaim)>::new();
    for plugin in &claiming {
        for (kind, declared) in &plugin.manifest.peripherals {
            if SYSTEM_PERIPHERALS.contains(&kind.as_str()) {
                return Err(TaskError::Task(format!(
                    "Plugin `{}` claims `{kind}`, which System owns",
                    plugin.manifest.id()
                )));
            }
            for (slot, claim) in declared {
                slots.push((
                    (plugin.manifest.id().to_owned(), kind.clone(), slot.clone()),
                    *claim,
                ));
            }
        }
    }

    let mut taken = BTreeMap::<String, String>::new();
    let mut filled = BTreeMap::<Slot, Filled>::new();
    let take = |taken: &mut BTreeMap<String, String>,
                device: &ClaimedDevice,
                owner: &Slot|
     -> Result<(), TaskError> {
        let (plugin, kind, slot) = owner;
        if let Some(previous) = taken.insert(device.name.clone(), format!("{plugin}.{kind}.{slot}"))
        {
            return Err(TaskError::Task(format!(
                "Board `{board_name}` gives device `{}` to both `{previous}` and `{plugin}.{kind}.{slot}`",
                device.name
            )));
        }
        Ok(())
    };

    // 1. Wiring from the Board.
    for id in board.configured_plugins() {
        let Some(plugin) = plugins.iter().find(|plugin| plugin.manifest.id() == id) else {
            continue;
        };
        for (key, device) in board.plugin_peripherals(id) {
            let context = || format!("Board `{board_name}` wires `plugins.{id}.peripherals.{key}`");
            let (kind, slot) = key.split_once('.').ok_or_else(|| {
                TaskError::Task(format!("{}: expected `<kind>.<slot>`", context()))
            })?;
            let claim = plugin
                .manifest
                .peripherals
                .get(kind)
                .and_then(|slots| slots.get(slot))
                .ok_or_else(|| {
                    TaskError::Task(format!("{}, a slot the Plugin does not declare", context()))
                })?;
            if claim.fill == PeripheralFill::Rest {
                return Err(TaskError::Task(format!(
                    "{}, but `*` slots take every unclaimed device and cannot be wired",
                    context()
                )));
            }
            let claimed = devices.device(kind, device).ok_or_else(|| {
                TaskError::Task(format!(
                    "{} to `{device}`, which is {}",
                    context(),
                    devices.kind_of(device).map_or_else(
                        || String::from("not a device of this Board"),
                        |other| format!("a `{other}` device, not `{kind}`")
                    )
                ))
            })?;
            if !enabled.contains(&id.to_owned()) {
                continue;
            }
            let owner = (id.to_owned(), kind.to_owned(), slot.to_owned());
            take(&mut taken, &claimed, &owner)?;
            filled.insert(owner, Filled::One(Some(claimed)));
        }
    }

    // 2. Primary, 3. any, 4. rest.
    for fill in [PeripheralFill::Primary, PeripheralFill::Any] {
        for (owner, _) in slots.iter().filter(|(_, claim)| claim.fill == fill) {
            if filled.contains_key(owner) {
                continue;
            }
            let (plugin, kind, slot) = owner;
            let candidate = match fill {
                PeripheralFill::Primary => devices.primary.get(kind).cloned(),
                _ => {
                    let free = devices
                        .by_kind
                        .get(kind)
                        .into_iter()
                        .flatten()
                        .filter(|device| !taken.contains_key(*device))
                        .cloned()
                        .collect::<Vec<_>>();
                    if free.len() > 1 {
                        return Err(TaskError::Task(format!(
                            "Board `{board_name}`: `{plugin}.{kind}.{slot}` takes any `{kind}`, and {} are free{}",
                            free.join(", "),
                            suggestion(plugin, kind, slot, &free)
                        )));
                    }
                    free.into_iter().next()
                }
            };
            let claimed = candidate.and_then(|name| devices.device(kind, &name));
            if let Some(device) = &claimed {
                take(&mut taken, device, owner)?;
            }
            filled.insert(owner.clone(), Filled::One(claimed));
        }
    }
    let mut rest_owner = BTreeMap::<&str, &Slot>::new();
    for (owner, _) in slots
        .iter()
        .filter(|(_, claim)| claim.fill == PeripheralFill::Rest)
    {
        let (plugin, kind, slot) = owner;
        if let Some(previous) = rest_owner.insert(kind.as_str(), owner) {
            return Err(TaskError::Task(format!(
                "`{}.{kind}.{}` and `{plugin}.{kind}.{slot}` both take every unclaimed `{kind}`; only one Plugin may",
                previous.0, previous.2
            )));
        }
    }
    for (kind, owner) in rest_owner {
        let mut rest = devices
            .by_kind
            .get(kind)
            .into_iter()
            .flatten()
            .filter(|device| !taken.contains_key(*device))
            .filter_map(|device| devices.device(kind, device))
            .collect::<Vec<_>>();
        // The primary device, when the slot receives it, comes first, so a
        // Plugin that defaults to its first device defaults to the primary.
        rest.sort_by_key(|device| devices.primary.get(kind) != Some(&device.name));
        filled.insert(owner.clone(), Filled::Many(rest));
    }

    // Required slots an enabled Plugin cannot do without.
    for (owner, claim) in &slots {
        let (plugin, kind, slot) = owner;
        if !claim.optional && matches!(filled.get(owner), Some(Filled::One(None)) | None) {
            let free = devices
                .by_kind
                .get(kind)
                .into_iter()
                .flatten()
                .filter(|device| !taken.contains_key(*device))
                .cloned()
                .collect::<Vec<_>>();
            return Err(TaskError::Task(format!(
                "Board `{board_name}` enables `{plugin}`, but its required slot `{kind}.{slot}` ({}) has no device{}",
                claim.fill.as_str(),
                if free.is_empty() {
                    format!("; the Board has no free `{kind}`")
                } else {
                    suggestion(plugin, kind, slot, &free)
                }
            )));
        }
    }

    let mut claims = Claims::new();
    for ((plugin, kind, slot), filled) in filled {
        claims
            .entry(plugin)
            .or_default()
            .insert((kind, slot), filled);
    }
    Ok(claims)
}
