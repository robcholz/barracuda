//! Plugin identity, registration, capability, and task lifecycle.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::{type_name, Any, TypeId};
use core::error::Error;
use core::fmt::{self, Debug};

use barracuda_kv::Database;
use barracuda_vfs::{FsError, ScopedVfs, Vfs};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_storage_async::nor_flash::NorFlash;
use getset::Getters;

use crate::storage::ScopedStorage;
use crate::{PluginStorage, StorageError};

/// Filesystem access requested by one Plugin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PluginFilesystem {
    /// The Plugin has no file namespace.
    #[default]
    None,
    /// The Plugin receives a private namespace on System's writable VFS.
    Private,
}

/// Machine-readable resources requested before Plugin registration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PluginRequirements {
    filesystem: PluginFilesystem,
}

impl PluginRequirements {
    /// Creates requirements with KV storage only.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            filesystem: PluginFilesystem::None,
        }
    }

    /// Declares the Plugin filesystem requirement.
    #[must_use]
    pub const fn with_filesystem(mut self, filesystem: PluginFilesystem) -> Self {
        self.filesystem = filesystem;
        self
    }

    /// Returns the declared filesystem requirement.
    #[must_use]
    pub const fn filesystem(&self) -> PluginFilesystem {
        self.filesystem
    }
}

/// Stable identity and persistent namespace of one Plugin.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(&'static str);

impl PluginId {
    /// Returns the Plugin identity as UTF-8 text.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure while validating a Plugin identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginIdError {
    /// The identity has no bytes.
    #[error("Plugin identity cannot be empty")]
    Empty,
}

impl TryFrom<&'static str> for PluginId {
    type Error = PluginIdError;

    fn try_from(value: &'static str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(PluginIdError::Empty);
        }
        Ok(Self(value))
    }
}

/// Result returned by Plugin initialization.
pub type PluginResult<T> = Result<T, PluginError>;

/// Failure while a Plugin initializes its capabilities or storage state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// Scoped persistent storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A typed capability could not be published or required.
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    /// The Plugin attempted to obtain VFS access without declaring it.
    #[error("Plugin did not declare private filesystem access")]
    FilesystemNotDeclared,
    /// System did not install an Embassy task spawner for Plugin startup.
    #[error("Plugin task spawner is unavailable during startup")]
    TaskSpawnerUnavailable,
    /// Plugin-specific initialization failed.
    #[error("Plugin initialization failed: {0}")]
    Registration(#[source] Box<dyn Error>),
}

impl PluginError {
    /// Wraps a Plugin-specific initialization error.
    #[must_use]
    pub fn registration(error: impl Error + 'static) -> Self {
        Self::Registration(Box::new(error))
    }
}

/// Static identity and dependency declaration shared by every frame size.
pub trait PluginDeclaration {
    /// Stable identity used for lifecycle tracking and storage.
    const ID: &'static str;

    /// Stable identities that must register before this Plugin.
    const DEPENDS_ON: &'static [&'static str] = &[];
}

/// One system-managed Plugin.
pub trait Plugin: PluginDeclaration {
    /// Resources that Plugin Manager must prepare before registration.
    const REQUIREMENTS: PluginRequirements = PluginRequirements::new();

    /// Registers the Plugin's capabilities and retained resources.
    ///
    /// Capability graph construction belongs exclusively to this phase. The
    /// default registration phase performs no work.
    fn register<Storage>(
        &mut self,
        _context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        Ok(())
    }

    /// Runs the Plugin's startup hook after every Plugin has registered.
    ///
    /// This hook cannot publish capabilities; that operation belongs to
    /// [`Self::register`]. A returned error causes the manager to roll back the
    /// Plugin's registered capabilities and retained resources.
    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        Ok(())
    }
}

trait ManagedPlugin<Storage: PluginStorage> {
    fn identity(&self) -> &'static str;
    fn dependencies(&self) -> &'static [&'static str];
    fn requirements(&self) -> PluginRequirements;
    fn register(&mut self, context: &mut PluginRegisterContext<'_, Storage>) -> PluginResult<()>;
    fn start(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>;
}

impl<T, Storage> ManagedPlugin<Storage> for T
where
    T: Plugin,
    Storage: PluginStorage,
{
    fn identity(&self) -> &'static str {
        T::ID
    }

    fn dependencies(&self) -> &'static [&'static str] {
        T::DEPENDS_ON
    }

    fn requirements(&self) -> PluginRequirements {
        T::REQUIREMENTS
    }

    fn register(&mut self, context: &mut PluginRegisterContext<'_, Storage>) -> PluginResult<()> {
        Plugin::register(self, context)
    }

    fn start(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()> {
        Plugin::start(self, context)
    }
}

/// Context provided exclusively during [`Plugin::register`].
#[derive(Getters)]
pub struct PluginRegisterContext<'a, Storage: PluginStorage> {
    /// Persistent typed key-value storage restricted to this Plugin's namespace.
    #[getset(get = "pub")]
    storage: Storage,
    filesystem: Option<ScopedVfs>,
    plugin_id: &'a PluginId,
    dependencies: &'a [PluginId],
    capabilities: &'a mut CapabilityRegistry,
    provided_capabilities: &'a mut Vec<CapabilityKey>,
    retained_resources: &'a mut Vec<Box<dyn Any>>,
}

impl<Storage: PluginStorage> PluginRegisterContext<'_, Storage> {
    /// Returns this Plugin's private VFS when it declared one.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::FilesystemNotDeclared`] for KV-only Plugins.
    pub fn filesystem(&self) -> PluginResult<&ScopedVfs> {
        self.filesystem
            .as_ref()
            .ok_or(PluginError::FilesystemNotDeclared)
    }

    /// Publishes one typed capability owned by the current Plugin.
    ///
    /// Consumers obtain the same shared value with [`Self::require`]. The
    /// capability is removed automatically when registration rolls back or the
    /// provider Plugin unloads.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError::AlreadyProvided`] when this Plugin already
    /// published the same concrete type.
    pub fn provide<T>(&mut self, capability: Rc<T>) -> PluginResult<()>
    where
        T: Any,
    {
        let key = CapabilityKey::new(self.plugin_id, TypeId::of::<T>());
        self.capabilities
            .insert(key.clone(), capability, type_name::<T>())?;
        self.provided_capabilities.push(key);
        Ok(())
    }

    /// Requires one typed capability from a declared provider dependency.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider identity is invalid, was not declared
    /// in [`PluginDeclaration::DEPENDS_ON`], or did not publish this concrete type.
    pub fn require<T>(&self, provider: &'static str) -> PluginResult<Rc<T>>
    where
        T: Any,
    {
        require_capability(self.dependencies, self.capabilities, provider)
    }

    /// Retains a resource for exactly the lifetime of the current Plugin.
    ///
    /// Registration guards can use `Drop` to undo entries installed into a
    /// required capability. Retained resources are dropped on rollback or after
    /// the Plugin unloads.
    pub fn retain<T>(&mut self, resource: T)
    where
        T: Any,
    {
        self.retained_resources.push(Box::new(resource));
    }
}

/// Hook-only context provided during [`Plugin::start`].
///
/// Capability publication is intentionally absent. The complete capability
/// graph must already exist before any startup hook runs.
#[derive(Getters)]
pub struct PluginStartContext<'a, Storage: PluginStorage> {
    /// Persistent typed key-value storage restricted to this Plugin's namespace.
    #[getset(get = "pub")]
    storage: Storage,
    filesystem: Option<ScopedVfs>,
    dependencies: &'a [PluginId],
    capabilities: &'a CapabilityRegistry,
    task_spawner: Option<Spawner>,
    retained_resources: &'a mut Vec<Box<dyn Any>>,
    task_cancellations: &'a mut Vec<PluginTaskCancellation>,
}

/// Cooperative cancellation signal tied to one Plugin-owned Embassy task.
///
/// A Plugin obtains this token during `start`, moves it into exactly one task,
/// and races [`Self::cancelled`] with that task's owner loop. Plugin Manager
/// signals the token when the Plugin unloads or startup rolls back.
pub struct PluginTaskToken {
    state: Arc<PluginTaskState>,
}

impl PluginTaskToken {
    /// Waits until Plugin Manager ends this task's lifecycle.
    pub async fn cancelled(&self) {
        self.state.cancellation.wait().await;
    }

    /// Returns whether cancellation has already been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancellation.signaled()
    }
}

impl Drop for PluginTaskToken {
    fn drop(&mut self) {
        self.state.completion.signal(());
    }
}

struct PluginTaskState {
    cancellation: Signal<CriticalSectionRawMutex, ()>,
    completion: Signal<CriticalSectionRawMutex, ()>,
}

struct PluginTaskCancellation {
    state: Arc<PluginTaskState>,
}

impl PluginTaskCancellation {
    async fn cancel_and_wait(self) {
        self.state.cancellation.signal(());
        self.state.completion.wait().await;
    }
}

impl Drop for PluginTaskCancellation {
    fn drop(&mut self) {
        self.state.cancellation.signal(());
    }
}

fn plugin_task_cancellation() -> (PluginTaskCancellation, PluginTaskToken) {
    let state = Arc::new(PluginTaskState {
        cancellation: Signal::new(),
        completion: Signal::new(),
    });
    (
        PluginTaskCancellation {
            state: Arc::clone(&state),
        },
        PluginTaskToken { state },
    )
}

impl<Storage: PluginStorage> PluginStartContext<'_, Storage> {
    /// Returns this Plugin's declared private VFS.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::FilesystemNotDeclared`] for KV-only Plugins.
    pub fn filesystem(&self) -> PluginResult<&ScopedVfs> {
        self.filesystem
            .as_ref()
            .ok_or(PluginError::FilesystemNotDeclared)
    }

    /// Requires one typed capability from a declared provider dependency.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is invalid, undeclared, or did not
    /// publish the requested capability during registration.
    pub fn require<T>(&self, provider: &'static str) -> PluginResult<Rc<T>>
    where
        T: Any,
    {
        require_capability(self.dependencies, self.capabilities, provider)
    }

    /// Returns the Embassy spawner installed by the System composition root.
    ///
    /// Task spawning is intentionally available only during Plugin startup,
    /// after the complete capability graph has registered.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::TaskSpawnerUnavailable`] when System did not
    /// install the executor spawner.
    pub fn task_spawner(&self) -> PluginResult<Spawner> {
        self.task_spawner.ok_or(PluginError::TaskSpawnerUnavailable)
    }

    /// Creates cancellation for one task and retains its owner with the Plugin.
    ///
    /// Call this once per permanent task, then move the returned token into that
    /// task. Dropping the Plugin lifecycle signals every token created here.
    pub fn task_token(&mut self) -> PluginTaskToken {
        let (owner, token) = plugin_task_cancellation();
        self.task_cancellations.push(owner);
        token
    }

    /// Retains a resource for exactly the lifetime of the current Plugin.
    pub fn retain<T>(&mut self, resource: T)
    where
        T: Any,
    {
        self.retained_resources.push(Box::new(resource));
    }
}

fn require_capability<T>(
    dependencies: &[PluginId],
    capabilities: &CapabilityRegistry,
    provider: &'static str,
) -> PluginResult<Rc<T>>
where
    T: Any,
{
    let provider =
        PluginId::try_from(provider).map_err(|source| CapabilityError::InvalidProvider {
            provider: provider.to_string(),
            source,
        })?;
    if !dependencies.contains(&provider) {
        return Err(CapabilityError::DependencyNotDeclared(provider).into());
    }
    capabilities.get::<T>(&provider).map_err(PluginError::from)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CapabilityKey {
    provider: PluginId,
    type_id: TypeId,
}

impl CapabilityKey {
    fn new(provider: &PluginId, type_id: TypeId) -> Self {
        Self {
            provider: provider.clone(),
            type_id,
        }
    }
}

#[derive(Default)]
struct CapabilityRegistry {
    entries: BTreeMap<CapabilityKey, Rc<dyn Any>>,
}

impl CapabilityRegistry {
    fn insert<T>(
        &mut self,
        key: CapabilityKey,
        capability: Rc<T>,
        capability_name: &'static str,
    ) -> Result<(), CapabilityError>
    where
        T: Any,
    {
        if self.entries.contains_key(&key) {
            return Err(CapabilityError::AlreadyProvided {
                provider: key.provider,
                capability: capability_name,
            });
        }
        let capability: Rc<dyn Any> = capability;
        self.entries.insert(key, capability);
        Ok(())
    }

    fn get<T>(&self, provider: &PluginId) -> Result<Rc<T>, CapabilityError>
    where
        T: Any,
    {
        let key = CapabilityKey::new(provider, TypeId::of::<T>());
        let capability =
            self.entries
                .get(&key)
                .cloned()
                .ok_or_else(|| CapabilityError::NotProvided {
                    provider: provider.clone(),
                    capability: type_name::<T>(),
                })?;
        capability
            .downcast::<T>()
            .map_err(|_capability| CapabilityError::TypeMismatch {
                provider: provider.clone(),
                capability: type_name::<T>(),
            })
    }

    fn remove_all(&mut self, keys: &[CapabilityKey]) {
        for key in keys {
            self.entries.remove(key);
        }
    }
}

/// Failure publishing or requiring a typed Plugin capability.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    /// A provider identity passed to `require` is invalid.
    #[error("Capability provider has an invalid identity: {provider}")]
    InvalidProvider {
        /// Invalid provider text.
        provider: String,
        /// Identity validation failure.
        #[source]
        source: PluginIdError,
    },
    /// The consumer did not declare the provider as a dependency.
    #[error("Plugin dependency was not declared: {0}")]
    DependencyNotDeclared(PluginId),
    /// The provider did not publish the requested concrete type.
    #[error("Plugin {provider} did not provide capability {capability}")]
    NotProvided {
        /// Provider Plugin identity.
        provider: PluginId,
        /// Requested Rust type name.
        capability: &'static str,
    },
    /// One provider attempted to publish the same concrete type twice.
    #[error("Plugin {provider} already provided capability {capability}")]
    AlreadyProvided {
        /// Provider Plugin identity.
        provider: PluginId,
        /// Duplicate Rust type name.
        capability: &'static str,
    },
    /// A stored capability did not match its `TypeId` key.
    #[error("Plugin {provider} capability had the wrong type for {capability}")]
    TypeMismatch {
        /// Provider Plugin identity.
        provider: PluginId,
        /// Requested Rust type name.
        capability: &'static str,
    },
}

struct LoadedPlugin<Storage: PluginStorage> {
    plugin: Box<dyn ManagedPlugin<Storage>>,
    started: bool,
    dependencies: Vec<PluginId>,
    provided_capabilities: Vec<CapabilityKey>,
    retained_resources: Vec<Box<dyn Any>>,
    task_cancellations: Vec<PluginTaskCancellation>,
    filesystem: Option<ScopedVfs>,
}

/// Failure while registering one Plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginRegisterError {
    /// The Plugin declared an invalid stable identity.
    #[error("Plugin declared an invalid identity: {0}")]
    InvalidId(#[from] PluginIdError),
    /// The Plugin identity is already registered.
    #[error("Plugin is already registered: {0}")]
    AlreadyRegistered(PluginId),
    /// One declared dependency has an invalid identity.
    #[error("Plugin dependency has an invalid identity: {dependency}")]
    InvalidDependency {
        /// Invalid dependency text.
        dependency: String,
        /// Identity validation failure.
        #[source]
        source: PluginIdError,
    },
    /// A Plugin declared itself as a dependency.
    #[error("Plugin cannot depend on itself: {0}")]
    SelfDependency(PluginId),
    /// A declared dependency has not been registered yet.
    #[error("Plugin dependency is not registered: {0}")]
    MissingDependency(PluginId),
    /// The queued Plugin graph contains a dependency cycle.
    #[error("Plugin dependency graph contains a cycle: {0:?}")]
    DependencyCycle(Vec<PluginId>),
    /// The Plugin declared a private VFS but System did not install one.
    #[error("Plugin {0} requires a private filesystem, but System did not install a VFS")]
    FilesystemUnavailable(PluginId),
    /// Plugin filesystem namespace preparation failed.
    #[error(transparent)]
    Filesystem(#[from] FsError),
    /// Plugin initialization failed and its published state was rolled back.
    #[error("Plugin registration failed: {0}")]
    Registration(#[source] PluginError),
}

/// Failure while starting one registered Plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginStartError {
    /// Plugin startup failed and its published state was rolled back.
    #[error("Plugin start failed: {0}")]
    Start(#[source] PluginError),
}

/// Failure while unloading one Plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginUnloadError {
    /// No loaded Plugin has this identity.
    #[error("Plugin is not loaded: {0}")]
    NotFound(PluginId),
    /// Loaded consumers still depend on this Plugin.
    #[error("Plugin {plugin} still has loaded dependents")]
    HasDependents {
        /// Provider that cannot yet be unloaded.
        plugin: PluginId,
        /// Loaded consumers, in stable identity order.
        dependents: Vec<PluginId>,
    },
}

/// Failure while opening Plugin Manager persistence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginManagerInitError {
    /// The shared key-value database could not be opened.
    #[error(transparent)]
    Database(#[from] barracuda_kv::OpenError),
}

/// Manages Plugin identities, scoped storage, capabilities, and owned tasks.
pub struct PluginManager<DatabaseRegion>
where
    DatabaseRegion: NorFlash + 'static,
    DatabaseRegion::Error: Debug,
{
    database: Rc<Database<DatabaseRegion>>,
    capabilities: CapabilityRegistry,
    task_spawner: Option<Spawner>,
    vfs_root: Option<Vfs>,
    loaded: BTreeMap<PluginId, LoadedPlugin<ScopedStorage<DatabaseRegion>>>,
    pending: BTreeMap<PluginId, Box<dyn ManagedPlugin<ScopedStorage<DatabaseRegion>>>>,
    registration_order: Vec<PluginId>,
}

impl<DatabaseRegion> PluginManager<DatabaseRegion>
where
    DatabaseRegion: NorFlash + 'static,
    DatabaseRegion::Error: Debug,
{
    /// Opens the Plugin database on a validated Barracuda partition.
    ///
    /// Plugin Manager owns database construction and only exposes scoped
    /// storage to Plugins.
    ///
    /// # Errors
    ///
    /// Returns an error when the partition geometry, I/O, or existing database
    /// contents are invalid.
    pub async fn open(region: DatabaseRegion) -> Result<Self, PluginManagerInitError> {
        let database = Database::open(region).await?;
        Ok(Self {
            database: Rc::new(database),
            capabilities: CapabilityRegistry::default(),
            task_spawner: None,
            vfs_root: None,
            loaded: BTreeMap::new(),
            pending: BTreeMap::new(),
            registration_order: Vec::new(),
        })
    }

    /// Installs a snapshot of System's writable global VFS namespace.
    pub fn install_vfs(&mut self, vfs: Vfs) {
        self.vfs_root = Some(vfs);
    }

    /// Installs the Embassy spawner exposed only to Plugin startup hooks.
    ///
    /// System calls this during composition before [`Self::start`]. The
    /// registration context deliberately has no access to the spawner, so
    /// Plugin tasks cannot begin before the complete graph is registered.
    pub fn install_task_spawner(&mut self, spawner: Spawner) {
        self.task_spawner = Some(spawner);
    }

    /// Runs one Plugin's registration phase and retains it for startup.
    ///
    /// Plugin initialization may use its durable scoped storage. If it fails,
    /// Capabilities and retained resources published by a failed registration
    /// are released. Persistent data is not deleted by rollback or unload.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate identity, Plugin
    /// initialization failure.
    pub fn register<T: Plugin + 'static>(&mut self, plugin: T) -> Result<(), PluginRegisterError> {
        self.register_managed(Box::new(plugin))
    }

    /// Adds one Plugin to the graph awaiting dependency-safe registration.
    ///
    /// System may add Plugins in any order. [`Self::register_all`] validates
    /// and scans the complete dependency graph before running registrations.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid, duplicate, or self-dependent identity.
    pub fn add<T: Plugin + 'static>(&mut self, plugin: T) -> Result<(), PluginRegisterError> {
        let id = PluginId::try_from(T::ID)?;
        if self.loaded.contains_key(&id) || self.pending.contains_key(&id) {
            log::warn!("refusing duplicate Plugin registration: {id}");
            return Err(PluginRegisterError::AlreadyRegistered(id));
        }
        validate_dependency_ids(&id, T::DEPENDS_ON)?;
        self.pending.insert(id, Box::new(plugin));
        Ok(())
    }

    /// Registers the complete queued Plugin graph in dependency order.
    ///
    /// # Errors
    ///
    /// Returns an error when a dependency is absent, the graph contains a
    /// cycle, or one Plugin registration fails.
    pub fn register_all(&mut self) -> Result<(), PluginRegisterError> {
        for plugin in self.pending.values() {
            for dependency in plugin.dependencies() {
                let dependency = PluginId::try_from(*dependency).map_err(|source| {
                    PluginRegisterError::InvalidDependency {
                        dependency: (*dependency).to_string(),
                        source,
                    }
                })?;
                if !self.loaded.contains_key(&dependency) && !self.pending.contains_key(&dependency)
                {
                    return Err(PluginRegisterError::MissingDependency(dependency));
                }
            }
        }
        while !self.pending.is_empty() {
            let ready = self
                .pending
                .iter()
                .find(|(_id, plugin)| {
                    plugin.dependencies().iter().all(|dependency| {
                        PluginId::try_from(*dependency)
                            .ok()
                            .is_some_and(|id| self.loaded.contains_key(&id))
                    })
                })
                .map(|(id, _plugin)| id.clone());
            let Some(id) = ready else {
                return Err(PluginRegisterError::DependencyCycle(
                    self.pending.keys().cloned().collect(),
                ));
            };
            let plugin = self
                .pending
                .remove(&id)
                .ok_or_else(|| PluginRegisterError::DependencyCycle(Vec::new()))?;
            self.register_managed(plugin)?;
        }
        Ok(())
    }

    fn register_managed(
        &mut self,
        mut plugin: Box<dyn ManagedPlugin<ScopedStorage<DatabaseRegion>>>,
    ) -> Result<(), PluginRegisterError> {
        let id = PluginId::try_from(plugin.identity())?;
        if self.loaded.contains_key(&id) {
            log::warn!("refusing duplicate Plugin registration: {id}");
            return Err(PluginRegisterError::AlreadyRegistered(id));
        }
        log::info!("registering Plugin {id}");

        let dependencies = resolve_dependencies(&id, plugin.dependencies(), &self.loaded)?;

        let filesystem = match plugin.requirements().filesystem() {
            PluginFilesystem::None => None,
            PluginFilesystem::Private => {
                let root = self
                    .vfs_root
                    .as_ref()
                    .ok_or_else(|| PluginRegisterError::FilesystemUnavailable(id.clone()))?;
                Some(crate::filesystem::scoped_filesystem(root, &id)?)
            }
        };

        let mut provided_capabilities = Vec::new();
        let mut retained_resources = Vec::new();
        let storage = ScopedStorage::new(Rc::clone(&self.database), &id);
        let result = {
            let mut context = PluginRegisterContext {
                storage,
                filesystem: filesystem.clone(),
                plugin_id: &id,
                dependencies: &dependencies,
                capabilities: &mut self.capabilities,
                provided_capabilities: &mut provided_capabilities,
                retained_resources: &mut retained_resources,
            };
            plugin.register(&mut context)
        };

        match result {
            Ok(()) => {
                log::info!("registered Plugin {id}");
                self.registration_order.push(id.clone());
                self.loaded.insert(
                    id,
                    LoadedPlugin {
                        plugin,
                        started: false,
                        dependencies,
                        provided_capabilities,
                        retained_resources,
                        task_cancellations: Vec::new(),
                        filesystem,
                    },
                );
                Ok(())
            }
            Err(source) => {
                log::error!("Plugin {id} registration failed: {source}");
                self.capabilities.remove_all(&provided_capabilities);
                Err(PluginRegisterError::Registration(source))
            }
        }
    }

    /// Starts every registered Plugin in dependency-safe registration order.
    ///
    /// # Errors
    ///
    /// Returns an error when one Plugin fails to start. State owned by the
    /// failing Plugin is rolled back.
    pub fn start(&mut self) -> Result<(), PluginStartError> {
        let order = self.registration_order.clone();
        for id in order {
            let Some(mut plugin) = self.loaded.remove(&id) else {
                continue;
            };
            if plugin.started {
                self.loaded.insert(id, plugin);
                continue;
            }
            log::info!("starting Plugin {id}");

            let storage = ScopedStorage::new(Rc::clone(&self.database), &id);
            let result = {
                let mut context = PluginStartContext {
                    storage,
                    filesystem: plugin.filesystem.clone(),
                    dependencies: &plugin.dependencies,
                    capabilities: &self.capabilities,
                    task_spawner: self.task_spawner,
                    retained_resources: &mut plugin.retained_resources,
                    task_cancellations: &mut plugin.task_cancellations,
                };
                plugin.plugin.start(&mut context)
            };

            match result {
                Ok(()) => {
                    log::info!("started Plugin {id}");
                    plugin.started = true;
                    self.loaded.insert(id, plugin);
                }
                Err(source) => {
                    log::error!("Plugin {id} start failed: {source}");
                    plugin.task_cancellations.clear();
                    self.capabilities.remove_all(&plugin.provided_capabilities);
                    self.registration_order
                        .retain(|registered| registered != &id);
                    return Err(PluginStartError::Start(source));
                }
            }
        }
        Ok(())
    }

    /// Unloads one Plugin.
    ///
    /// The Plugin's `ekv` namespace remains durable and will be reused if the
    /// same stable identity is loaded again. This signals every Plugin task and
    /// waits for its token to be dropped before releasing capabilities and
    /// retained resources.
    ///
    /// # Errors
    ///
    /// Returns an error when the Plugin is unknown or still has dependents.
    pub async fn unload(&mut self, id: &PluginId) -> Result<(), PluginUnloadError> {
        let dependents = self
            .loaded
            .iter()
            .filter(|(_consumer, plugin)| plugin.dependencies.contains(id))
            .map(|(consumer, _plugin)| consumer.clone())
            .collect::<Vec<_>>();
        if !dependents.is_empty() {
            return Err(PluginUnloadError::HasDependents {
                plugin: id.clone(),
                dependents,
            });
        }
        let Some(plugin) = self.loaded.remove(id) else {
            return Err(PluginUnloadError::NotFound(id.clone()));
        };
        let LoadedPlugin {
            provided_capabilities,
            retained_resources,
            task_cancellations,
            ..
        } = plugin;
        for cancellation in task_cancellations {
            cancellation.cancel_and_wait().await;
        }
        self.capabilities.remove_all(&provided_capabilities);
        drop(retained_resources);
        self.registration_order
            .retain(|registered| registered != id);
        Ok(())
    }

    /// Unloads the complete Plugin graph in reverse dependency order.
    ///
    /// Each Plugin follows the same task cancellation and retained-resource
    /// release path as [`Self::unload`].
    ///
    /// # Errors
    ///
    /// Returns the first Plugin unload failure. Plugins already unloaded before
    /// that failure remain unloaded.
    pub async fn shutdown(&mut self) -> Result<(), PluginUnloadError> {
        let order = self.registration_order.clone();
        for id in order.iter().rev() {
            self.unload(id).await?;
        }
        Ok(())
    }

    /// Returns whether a Plugin identity is currently loaded.
    #[must_use]
    pub fn is_loaded(&self, id: &PluginId) -> bool {
        self.loaded.contains_key(id)
    }
}

fn validate_dependency_ids(
    plugin: &PluginId,
    declared: &[&'static str],
) -> Result<(), PluginRegisterError> {
    for dependency in declared {
        let id = PluginId::try_from(*dependency).map_err(|source| {
            PluginRegisterError::InvalidDependency {
                dependency: (*dependency).to_string(),
                source,
            }
        })?;
        if id == *plugin {
            return Err(PluginRegisterError::SelfDependency(id));
        }
    }
    Ok(())
}

fn resolve_dependencies<Storage: PluginStorage>(
    plugin: &PluginId,
    declared: &[&'static str],
    loaded: &BTreeMap<PluginId, LoadedPlugin<Storage>>,
) -> Result<Vec<PluginId>, PluginRegisterError> {
    let mut dependencies = BTreeSet::new();
    for dependency in declared {
        let id = PluginId::try_from(*dependency).map_err(|source| {
            PluginRegisterError::InvalidDependency {
                dependency: (*dependency).to_string(),
                source,
            }
        })?;
        if id == *plugin {
            return Err(PluginRegisterError::SelfDependency(id));
        }
        if !loaded.contains_key(&id) {
            return Err(PluginRegisterError::MissingDependency(id));
        }
        dependencies.insert(id);
    }
    Ok(dependencies.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::plugin_task_cancellation;

    #[test]
    fn dropping_a_plugin_task_owner_wakes_its_task() {
        let (owner, token) = plugin_task_cancellation();
        drop(owner);

        block_on(token.cancelled());
    }
}
