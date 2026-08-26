//! Plugin identity, registration, and grouped Component lifecycle.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::{type_name, Any, TypeId};
use core::error::Error;
use core::fmt::{self, Debug};

use barracuda_event_router::{Component, ComponentId, EventRouter, LoadError, UnloadError};
use barracuda_kv::Database;
use barracuda_vfs::{FsError, Vfs};
use embassy_executor::Spawner;
use embedded_storage_async::nor_flash::NorFlash;
use getset::Getters;

use crate::storage::ScopedStorage;
use crate::{PluginStorage, PluginVfs, StorageError};

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

/// Failure while a Plugin initializes its Components or storage state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// Event Router rejected one Component.
    #[error(transparent)]
    Component(#[from] LoadError),
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

/// One system-managed Plugin that may register multiple Components.
pub trait Plugin<const M: usize> {
    /// Stable identities of Plugins that must already be registered.
    const DEPENDS_ON: &'static [&'static str] = &[];

    /// Resources that Plugin Manager must prepare before registration.
    const REQUIREMENTS: PluginRequirements = PluginRequirements::new();

    /// Returns the stable identity used for lifecycle tracking and storage.
    fn id(&self) -> &'static str;

    /// Registers the Plugin's capabilities and Event Router Components.
    ///
    /// Component graph construction belongs exclusively to this phase. The
    /// default registration phase performs no work.
    fn register<Storage>(
        &mut self,
        _context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        Ok(())
    }

    /// Runs the Plugin's startup hook after every Plugin has registered.
    ///
    /// This hook cannot load Event Router Components or publish capabilities;
    /// those operations belong to [`Self::register`]. A returned error causes
    /// the manager to unload the Plugin's registered Components in reverse
    /// order.
    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        Ok(())
    }
}

trait ManagedPlugin<const M: usize, Storage: PluginStorage> {
    fn start(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>;
}

impl<T, const M: usize, Storage> ManagedPlugin<M, Storage> for T
where
    T: Plugin<M>,
    Storage: PluginStorage,
{
    fn start(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()> {
        Plugin::start(self, context)
    }
}

trait ComponentRegistrar<const M: usize> {
    fn register_component(
        &mut self,
        component: Box<dyn Component<M>>,
    ) -> Result<ComponentId, LoadError>;
}

struct EventRouterRegistrar<'a, const N: usize, const M: usize, const Q: usize> {
    router: &'a mut EventRouter<N, M, Q>,
}

impl<const N: usize, const M: usize, const Q: usize> ComponentRegistrar<M>
    for EventRouterRegistrar<'_, N, M, Q>
{
    fn register_component(
        &mut self,
        component: Box<dyn Component<M>>,
    ) -> Result<ComponentId, LoadError> {
        self.router.load(component)
    }
}

/// Registration-only access to Event Router Component loading.
pub struct PluginEventRouterContext<'a, const M: usize> {
    registrar: &'a mut dyn ComponentRegistrar<M>,
    component_ids: &'a mut Vec<ComponentId>,
}

impl<const M: usize> PluginEventRouterContext<'_, M> {
    /// Loads one Component owned by the registering Plugin.
    ///
    /// The manager records the returned identity for Plugin-wide rollback and
    /// unload.
    ///
    /// # Errors
    ///
    /// Returns an error when Event Router cannot load the Component.
    pub fn load<C>(&mut self, component: C) -> PluginResult<ComponentId>
    where
        C: Component<M> + 'static,
    {
        let id = self.registrar.register_component(Box::new(component))?;
        self.component_ids.push(id);
        Ok(id)
    }
}

/// Context provided exclusively during [`Plugin::register`].
#[derive(Getters)]
pub struct PluginRegisterContext<'a, const M: usize, Storage: PluginStorage> {
    /// Explicit Event Router registration boundary.
    pub event_router: PluginEventRouterContext<'a, M>,
    /// Persistent typed key-value storage restricted to this Plugin's namespace.
    #[getset(get = "pub")]
    storage: Storage,
    filesystem: Option<PluginVfs>,
    plugin_id: &'a PluginId,
    dependencies: &'a [PluginId],
    capabilities: &'a mut CapabilityRegistry,
    provided_capabilities: &'a mut Vec<CapabilityKey>,
    retained_resources: &'a mut Vec<Box<dyn Any>>,
}

impl<const M: usize, Storage: PluginStorage> PluginRegisterContext<'_, M, Storage> {
    /// Returns this Plugin's private VFS when it declared one.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::FilesystemNotDeclared`] for KV-only Plugins.
    pub fn filesystem(&self) -> PluginResult<&PluginVfs> {
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
    /// in [`Plugin::DEPENDS_ON`], or did not publish this concrete type.
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
    /// the Plugin's Components unload successfully.
    pub fn retain<T>(&mut self, resource: T)
    where
        T: Any,
    {
        self.retained_resources.push(Box::new(resource));
    }
}

/// Hook-only context provided during [`Plugin::start`].
///
/// Event Router loading and capability publication are intentionally absent.
/// The complete Component and capability graph must already exist before any
/// startup hook runs.
///
/// ```compile_fail
/// use barracuda_plugin_manager::{PluginStartContext, PluginStorage};
///
/// fn load_late<Storage: PluginStorage>(context: &mut PluginStartContext<'_, Storage>) {
///     let _router = &mut context.event_router;
/// }
/// ```
#[derive(Getters)]
pub struct PluginStartContext<'a, Storage: PluginStorage> {
    /// Persistent typed key-value storage restricted to this Plugin's namespace.
    #[getset(get = "pub")]
    storage: Storage,
    filesystem: Option<PluginVfs>,
    dependencies: &'a [PluginId],
    capabilities: &'a CapabilityRegistry,
    task_spawner: Option<Spawner>,
    retained_resources: &'a mut Vec<Box<dyn Any>>,
}

impl<Storage: PluginStorage> PluginStartContext<'_, Storage> {
    /// Returns this Plugin's declared private VFS.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::FilesystemNotDeclared`] for KV-only Plugins.
    pub fn filesystem(&self) -> PluginResult<&PluginVfs> {
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
    /// after the complete capability and Component graph has registered.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::TaskSpawnerUnavailable`] when System did not
    /// install the executor spawner.
    pub fn task_spawner(&self) -> PluginResult<Spawner> {
        self.task_spawner.ok_or(PluginError::TaskSpawnerUnavailable)
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

struct LoadedPlugin<const M: usize, Storage: PluginStorage> {
    plugin: Box<dyn ManagedPlugin<M, Storage>>,
    available: bool,
    started: bool,
    component_ids: Vec<ComponentId>,
    dependencies: Vec<PluginId>,
    provided_capabilities: Vec<CapabilityKey>,
    retained_resources: Vec<Box<dyn Any>>,
    filesystem: Option<PluginVfs>,
}

/// Component cleanup failure associated with a Plugin lifecycle operation.
#[derive(Debug)]
pub struct PluginComponentCleanupFailure {
    /// Component whose cleanup remains incomplete.
    pub id: ComponentId,
    /// Event Router unload failure.
    pub error: UnloadError,
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
    /// The Plugin declared a private VFS but System did not install one.
    #[error("Plugin {0} requires a private filesystem, but System did not install a VFS")]
    FilesystemUnavailable(PluginId),
    /// Plugin filesystem namespace preparation failed.
    #[error(transparent)]
    Filesystem(#[from] FsError),
    /// Plugin initialization failed and every Component was rolled back.
    #[error("Plugin registration failed: {0}")]
    Registration(#[source] PluginError),
    /// Plugin initialization failed and at least one Component could not be rolled back.
    #[error("Plugin registration failed and Component rollback was incomplete")]
    Rollback {
        /// Original Plugin initialization failure.
        #[source]
        source: PluginError,
        /// Component cleanup failures retained for a later unload retry.
        cleanup: Vec<PluginComponentCleanupFailure>,
    },
}

/// Failure while starting one registered Plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginStartError {
    /// Plugin startup failed and every Component was rolled back.
    #[error("Plugin start failed: {0}")]
    Start(#[source] PluginError),
    /// Plugin startup failed and at least one Component could not be rolled back.
    #[error("Plugin start failed and Component rollback was incomplete")]
    Rollback {
        /// Original Plugin startup failure.
        #[source]
        source: PluginError,
        /// Component cleanup failures retained for a later unload retry.
        cleanup: Vec<PluginComponentCleanupFailure>,
    },
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
    /// At least one owned Component could not be unloaded.
    #[error("Plugin Component cleanup was incomplete")]
    Cleanup(Vec<PluginComponentCleanupFailure>),
}

/// Failure while opening Plugin Manager persistence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginManagerInitError {
    /// The shared key-value database could not be opened.
    #[error(transparent)]
    Database(#[from] barracuda_kv::OpenError),
}

/// Manages Plugin identities, scoped storage, and grouped Components.
///
/// Event Router continues to own Component execution. The manager only uses a
/// mutable Router reference while registering, starting, or unloading a Plugin.
pub struct PluginManager<const M: usize, DatabaseRegion>
where
    DatabaseRegion: NorFlash + 'static,
    DatabaseRegion::Error: Debug,
{
    database: Rc<Database<DatabaseRegion>>,
    capabilities: CapabilityRegistry,
    task_spawner: Option<Spawner>,
    vfs_root: Option<Vfs>,
    loaded: BTreeMap<PluginId, LoadedPlugin<M, ScopedStorage<DatabaseRegion>>>,
    registration_order: Vec<PluginId>,
}

impl<const M: usize, DatabaseRegion> PluginManager<M, DatabaseRegion>
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
    /// Components already registered by that call are unloaded in reverse
    /// order. Persistent data is not deleted by rollback or unload.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate identity, Plugin
    /// initialization failure, or incomplete Component rollback.
    pub fn register<const N: usize, const Q: usize, T: Plugin<M> + 'static>(
        &mut self,
        router: &mut EventRouter<N, M, Q>,
        mut plugin: T,
    ) -> Result<(), PluginRegisterError> {
        let id = PluginId::try_from(plugin.id())?;
        if self.loaded.contains_key(&id) {
            log::warn!("refusing duplicate Plugin registration: {id}");
            return Err(PluginRegisterError::AlreadyRegistered(id));
        }
        log::info!("registering Plugin {id}");

        let dependencies =
            resolve_dependencies::<M, T, ScopedStorage<DatabaseRegion>>(&id, &self.loaded)?;

        let filesystem = match T::REQUIREMENTS.filesystem() {
            PluginFilesystem::None => None,
            PluginFilesystem::Private => {
                let root = self
                    .vfs_root
                    .as_ref()
                    .ok_or_else(|| PluginRegisterError::FilesystemUnavailable(id.clone()))?;
                let source_root = format!("/plugins/{id}");
                Some(root.scoped(&source_root)?)
            }
        };

        let mut component_ids = Vec::new();
        let mut provided_capabilities = Vec::new();
        let mut retained_resources = Vec::new();
        let storage = ScopedStorage::new(Rc::clone(&self.database), &id);
        let result = {
            let mut registrar = EventRouterRegistrar { router };
            let mut context = PluginRegisterContext {
                event_router: PluginEventRouterContext {
                    registrar: &mut registrar,
                    component_ids: &mut component_ids,
                },
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
                        plugin: Box::new(plugin),
                        available: true,
                        started: false,
                        component_ids,
                        dependencies,
                        provided_capabilities,
                        retained_resources,
                        filesystem,
                    },
                );
                Ok(())
            }
            Err(source) => {
                log::error!("Plugin {id} registration failed: {source}");
                let (remaining, cleanup) = rollback_components(router, component_ids);
                if cleanup.is_empty() {
                    self.capabilities.remove_all(&provided_capabilities);
                    Err(PluginRegisterError::Registration(source))
                } else {
                    self.loaded.insert(
                        id,
                        LoadedPlugin {
                            plugin: Box::new(plugin),
                            available: false,
                            started: false,
                            component_ids: remaining,
                            dependencies,
                            provided_capabilities,
                            retained_resources,
                            filesystem,
                        },
                    );
                    Err(PluginRegisterError::Rollback { source, cleanup })
                }
            }
        }
    }

    /// Starts every registered Plugin in dependency-safe registration order.
    ///
    /// # Errors
    ///
    /// Returns an error when one Plugin fails to start. Components owned by the
    /// failing Plugin are rolled back in reverse registration order.
    pub fn start<const N: usize, const Q: usize>(
        &mut self,
        router: &mut EventRouter<N, M, Q>,
    ) -> Result<(), PluginStartError> {
        let order = self.registration_order.clone();
        for id in order {
            let Some(mut plugin) = self.loaded.remove(&id) else {
                continue;
            };
            if plugin.started || !plugin.available {
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
                    let (remaining, cleanup) = rollback_components(router, plugin.component_ids);
                    if cleanup.is_empty() {
                        self.capabilities.remove_all(&plugin.provided_capabilities);
                        self.registration_order
                            .retain(|registered| registered != &id);
                        return Err(PluginStartError::Start(source));
                    }
                    plugin.available = false;
                    plugin.component_ids = remaining;
                    self.loaded.insert(id, plugin);
                    return Err(PluginStartError::Rollback { source, cleanup });
                }
            }
        }
        Ok(())
    }

    /// Unloads every Component owned by one Plugin in reverse registration order.
    ///
    /// The Plugin's `ekv` namespace remains durable and will be reused if the
    /// same stable identity is loaded again.
    ///
    /// # Errors
    ///
    /// Returns an error when the Plugin is unknown or Component cleanup is
    /// incomplete. Failed Component identities remain tracked for retry.
    pub fn unload<const N: usize, const Q: usize>(
        &mut self,
        router: &mut EventRouter<N, M, Q>,
        id: &PluginId,
    ) -> Result<(), PluginUnloadError> {
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
            plugin: managed_plugin,
            available,
            started,
            component_ids,
            dependencies,
            provided_capabilities,
            retained_resources,
            filesystem,
        } = plugin;
        let (remaining, cleanup) = rollback_components(router, component_ids);
        if cleanup.is_empty() {
            self.capabilities.remove_all(&provided_capabilities);
            drop(retained_resources);
            self.registration_order
                .retain(|registered| registered != id);
            Ok(())
        } else {
            self.loaded.insert(
                id.clone(),
                LoadedPlugin {
                    plugin: managed_plugin,
                    available,
                    started,
                    component_ids: remaining,
                    dependencies,
                    provided_capabilities,
                    retained_resources,
                    filesystem,
                },
            );
            Err(PluginUnloadError::Cleanup(cleanup))
        }
    }

    /// Returns whether a Plugin identity is currently loaded.
    #[must_use]
    pub fn is_loaded(&self, id: &PluginId) -> bool {
        self.loaded.contains_key(id)
    }

    /// Returns Component identities owned by one loaded Plugin.
    #[must_use]
    pub fn component_ids(&self, id: &PluginId) -> Option<&[ComponentId]> {
        self.loaded
            .get(id)
            .map(|plugin| plugin.component_ids.as_slice())
    }
}

fn resolve_dependencies<const M: usize, T: Plugin<M>, Storage: PluginStorage>(
    plugin: &PluginId,
    loaded: &BTreeMap<PluginId, LoadedPlugin<M, Storage>>,
) -> Result<Vec<PluginId>, PluginRegisterError> {
    let mut dependencies = BTreeSet::new();
    for dependency in T::DEPENDS_ON {
        let id = PluginId::try_from(*dependency).map_err(|source| {
            PluginRegisterError::InvalidDependency {
                dependency: (*dependency).to_string(),
                source,
            }
        })?;
        if id == *plugin {
            return Err(PluginRegisterError::SelfDependency(id));
        }
        if !loaded.get(&id).is_some_and(|plugin| plugin.available) {
            return Err(PluginRegisterError::MissingDependency(id));
        }
        dependencies.insert(id);
    }
    Ok(dependencies.into_iter().collect())
}

fn rollback_components<const N: usize, const M: usize, const Q: usize>(
    router: &mut EventRouter<N, M, Q>,
    component_ids: Vec<ComponentId>,
) -> (Vec<ComponentId>, Vec<PluginComponentCleanupFailure>) {
    let mut remaining = Vec::new();
    let mut cleanup = Vec::new();
    for id in component_ids.into_iter().rev() {
        if let Err(error) = router.unload(id) {
            remaining.push(id);
            cleanup.push(PluginComponentCleanupFailure { id, error });
        }
    }
    (remaining, cleanup)
}
