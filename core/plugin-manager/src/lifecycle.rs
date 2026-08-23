//! Plugin identity, registration, and grouped Component lifecycle.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::{type_name, Any, TypeId};
use core::error::Error;
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use barracuda_event_router::{Component, ComponentId, EventRouter, LoadError, UnloadError};
use ekv::flash::Flash;
use embassy_sync_06::blocking_mutex::raw::RawMutex;
use getset::Getters;

use crate::storage::StorageBackend;
use crate::{EkvStore, ScopedStorage, StorageError};

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

/// Cooperative future returned by [`Plugin::register`].
pub type PluginRegisterFuture<'a> = Pin<Box<dyn Future<Output = PluginResult<()>> + 'a>>;

/// Cooperative future returned by [`Plugin::start`].
pub type PluginStartFuture<'a> = Pin<Box<dyn Future<Output = PluginResult<()>> + 'a>>;

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

    /// Returns the stable identity used for lifecycle tracking and storage.
    fn id(&self) -> &'static str;

    /// Registers Plugin-owned state before any Plugin starts.
    ///
    /// The default registration phase performs no work.
    fn register<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, M>,
    ) -> PluginRegisterFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    /// Starts the Plugin after every Plugin has registered.
    ///
    /// The Plugin may clone its scoped storage into any registered Component.
    /// A returned error causes the manager to unload Components registered by
    /// this call in reverse order.
    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a>;
}

trait ManagedPlugin<const M: usize> {
    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a>;
}

impl<P, const M: usize> ManagedPlugin<M> for P
where
    P: Plugin<M>,
{
    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
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

/// Capabilities provided while one Plugin initializes.
#[derive(Getters)]
pub struct PluginContext<'a, const M: usize> {
    registrar: &'a mut dyn ComponentRegistrar<M>,
    /// Persistent raw byte storage restricted to this Plugin's namespace.
    #[getset(get = "pub")]
    storage: ScopedStorage,
    plugin_id: &'a PluginId,
    dependencies: &'a [PluginId],
    capabilities: &'a mut CapabilityRegistry,
    component_ids: &'a mut Vec<ComponentId>,
    provided_capabilities: &'a mut Vec<CapabilityKey>,
    retained_resources: &'a mut Vec<Box<dyn Any>>,
}

impl<const M: usize> PluginContext<'_, M> {
    /// Loads one Component owned by the current Plugin.
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
        let provider =
            PluginId::try_from(provider).map_err(|source| CapabilityError::InvalidProvider {
                provider: provider.to_string(),
                source,
            })?;
        if !self.dependencies.contains(&provider) {
            return Err(CapabilityError::DependencyNotDeclared(provider).into());
        }
        self.capabilities
            .get::<T>(&provider)
            .map_err(PluginError::from)
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

struct LoadedPlugin<const M: usize> {
    plugin: Box<dyn ManagedPlugin<M>>,
    available: bool,
    started: bool,
    component_ids: Vec<ComponentId>,
    dependencies: Vec<PluginId>,
    provided_capabilities: Vec<CapabilityKey>,
    retained_resources: Vec<Box<dyn Any>>,
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

/// Manages Plugin identities, scoped storage, and grouped Components.
///
/// Event Router continues to own Component execution. The manager only uses a
/// mutable Router reference while registering, starting, or unloading a Plugin.
pub struct PluginManager<const M: usize> {
    backend: Rc<dyn StorageBackend>,
    capabilities: CapabilityRegistry,
    loaded: BTreeMap<PluginId, LoadedPlugin<M>>,
    registration_order: Vec<PluginId>,
}

impl<const M: usize> PluginManager<M> {
    /// Creates an empty manager backed by one shared `ekv` database.
    #[must_use]
    pub fn new<F, Mutex>(store: EkvStore<F, Mutex>) -> Self
    where
        F: Flash + 'static,
        Mutex: RawMutex + 'static,
    {
        let backend: Rc<dyn StorageBackend> = Rc::new(store);
        Self {
            backend,
            capabilities: CapabilityRegistry::default(),
            loaded: BTreeMap::new(),
            registration_order: Vec::new(),
        }
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
    pub async fn register<const N: usize, const Q: usize, P: Plugin<M> + 'static>(
        &mut self,
        router: &mut EventRouter<N, M, Q>,
        mut plugin: P,
    ) -> Result<(), PluginRegisterError> {
        let id = PluginId::try_from(plugin.id())?;
        if self.loaded.contains_key(&id) {
            return Err(PluginRegisterError::AlreadyRegistered(id));
        }

        let dependencies = resolve_dependencies::<M, P>(&id, &self.loaded)?;

        let mut component_ids = Vec::new();
        let mut provided_capabilities = Vec::new();
        let mut retained_resources = Vec::new();
        let storage = ScopedStorage::new(Rc::clone(&self.backend), &id);
        let result = {
            let mut registrar = EventRouterRegistrar { router };
            let mut context = PluginContext {
                registrar: &mut registrar,
                storage,
                plugin_id: &id,
                dependencies: &dependencies,
                capabilities: &mut self.capabilities,
                component_ids: &mut component_ids,
                provided_capabilities: &mut provided_capabilities,
                retained_resources: &mut retained_resources,
            };
            plugin.register(&mut context).await
        };

        match result {
            Ok(()) => {
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
                    },
                );
                Ok(())
            }
            Err(source) => {
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
    pub async fn start<const N: usize, const Q: usize>(
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

            let storage = ScopedStorage::new(Rc::clone(&self.backend), &id);
            let result = {
                let mut registrar = EventRouterRegistrar { router };
                let mut context = PluginContext {
                    registrar: &mut registrar,
                    storage,
                    plugin_id: &id,
                    dependencies: &plugin.dependencies,
                    capabilities: &mut self.capabilities,
                    component_ids: &mut plugin.component_ids,
                    provided_capabilities: &mut plugin.provided_capabilities,
                    retained_resources: &mut plugin.retained_resources,
                };
                plugin.plugin.start(&mut context).await
            };

            match result {
                Ok(()) => {
                    plugin.started = true;
                    self.loaded.insert(id, plugin);
                }
                Err(source) => {
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

fn resolve_dependencies<const M: usize, P: Plugin<M>>(
    plugin: &PluginId,
    loaded: &BTreeMap<PluginId, LoadedPlugin<M>>,
) -> Result<Vec<PluginId>, PluginRegisterError> {
    let mut dependencies = BTreeSet::new();
    for dependency in P::DEPENDS_ON {
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
