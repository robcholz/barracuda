//! Plugin lifecycle integration tests against Event Router and `ekv`.

#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, MemFs, RegisterContext,
    RpcLaneStorage, RunContext, UnregisterContext,
};
use barracuda_plugin_manager::{
    CapabilityError, EkvStore, NoopRawMutex, Plugin, PluginContext, PluginError, PluginId,
    PluginIdError, PluginManager, PluginRegisterError, PluginRegisterFuture, PluginStartError,
    PluginStartFuture, PluginUnloadError, ScopedStorage, StorageMutation,
};
use ekv::{flash::MemFlash, Config};
use futures_lite::future::block_on;

const FRAME_SIZE: usize = 64;

fn store() -> EkvStore<MemFlash, NoopRawMutex> {
    let store = EkvStore::new(MemFlash::new(), Config::default());
    block_on(store.format()).expect("format test store");
    store
}

fn router() -> EventRouter<8, FRAME_SIZE, 8> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let filesystem = Box::leak(Box::new(MemFs::new()));
    EventRouter::new(lanes, filesystem, "workflows").expect("create Event Router")
}

struct PendingComponent {
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
    storage: ScopedStorage,
}

impl Component<FRAME_SIZE> for PendingComponent {
    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        self.registered.set(self.registered.get() + 1);
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        let _storage = self.storage.clone();
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.unregistered.set(self.unregistered.get() + 1);
        Ok(())
    }
}

struct StatefulPlugin {
    id: &'static str,
    value: &'static [u8],
    observed: Rc<RefCell<Option<Vec<u8>>>>,
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
    component_count: usize,
}

struct IdentifiedPlugin(&'static str);

impl Plugin<FRAME_SIZE> for IdentifiedPlugin {
    fn id(&self) -> &'static str {
        self.0
    }

    fn start<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn register_uses_the_identity_declared_by_the_plugin() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let id = PluginId::try_from("identified").unwrap();

    block_on(manager.register(&mut router, IdentifiedPlugin("identified"))).unwrap();

    assert!(manager.is_loaded(&id));
}

impl Plugin<FRAME_SIZE> for StatefulPlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            *self.observed.borrow_mut() = context.storage().get(b"state").await?;
            context.storage().put(b"state", self.value).await?;

            for _ in 0..self.component_count {
                context.load(PendingComponent {
                    registered: Rc::clone(&self.registered),
                    unregistered: Rc::clone(&self.unregistered),
                    storage: context.storage().clone(),
                })?;
            }
            Ok(())
        })
    }
}

#[test]
fn plugin_id_rejects_only_an_empty_identity() {
    let id = PluginId::try_from("scheduler").unwrap();
    assert_eq!(id.as_str(), "scheduler");
    assert_eq!(id.to_string(), "scheduler");
    assert_eq!(PluginId::try_from(""), Err(PluginIdError::Empty));

    let long_id: &'static str = Box::leak("x".repeat(1024).into_boxed_str());
    assert_eq!(PluginId::try_from(long_id).unwrap().as_str(), long_id);
}

#[test]
fn registering_a_plugin_with_an_invalid_identity_is_rejected() {
    let mut manager = PluginManager::new(store());
    let mut router = router();

    let error = block_on(manager.register(&mut router, IdentifiedPlugin(""))).unwrap_err();

    assert!(matches!(
        error,
        PluginRegisterError::InvalidId(PluginIdError::Empty)
    ));
}

#[test]
fn unloading_an_unknown_plugin_is_rejected() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let id = PluginId::try_from("unknown").unwrap();

    let error = manager.unload(&mut router, &id).unwrap_err();

    assert!(matches!(
        error,
        barracuda_plugin_manager::PluginUnloadError::NotFound(found) if found == id
    ));
}

#[test]
fn plugins_have_isolated_durable_scopes() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let scheduler_observed = Rc::new(RefCell::new(None));
    let other_observed = Rc::new(RefCell::new(None));

    block_on(manager.register(
        &mut router,
        StatefulPlugin {
            id: "scheduler",
            value: b"scheduler-state",
            observed: Rc::clone(&scheduler_observed),
            registered: Rc::clone(&registered),
            unregistered: Rc::clone(&unregistered),
            component_count: 1,
        },
    ))
    .unwrap();
    block_on(manager.register(
        &mut router,
        StatefulPlugin {
            id: "other",
            value: b"other-state",
            observed: Rc::clone(&other_observed),
            registered: Rc::clone(&registered),
            unregistered: Rc::clone(&unregistered),
            component_count: 1,
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    assert_eq!(*scheduler_observed.borrow(), None);
    assert_eq!(*other_observed.borrow(), None);

    manager
        .unload(&mut router, &PluginId::try_from("scheduler").unwrap())
        .unwrap();
    let restored = Rc::new(RefCell::new(None));
    block_on(manager.register(
        &mut router,
        StatefulPlugin {
            id: "scheduler",
            value: b"updated",
            observed: Rc::clone(&restored),
            registered: Rc::clone(&registered),
            unregistered: Rc::clone(&unregistered),
            component_count: 1,
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    assert_eq!(
        restored.borrow().as_deref(),
        Some(b"scheduler-state".as_slice())
    );
}

#[test]
fn one_plugin_can_register_multiple_components() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let id = PluginId::try_from("multi").unwrap();

    block_on(manager.register(
        &mut router,
        StatefulPlugin {
            id: "multi",
            value: b"value",
            observed: Rc::new(RefCell::new(None)),
            registered: Rc::clone(&registered),
            unregistered: Rc::clone(&unregistered),
            component_count: 3,
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    assert_eq!(registered.get(), 3);
    assert_eq!(manager.component_ids(&id).unwrap().len(), 3);
    manager.unload(&mut router, &id).unwrap();
    assert_eq!(unregistered.get(), 3);
}

#[test]
fn duplicate_plugin_id_is_rejected() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let id = PluginId::try_from("duplicate").unwrap();
    let make_plugin = || StatefulPlugin {
        id: "duplicate",
        value: b"value",
        observed: Rc::new(RefCell::new(None)),
        registered: Rc::new(Cell::new(0)),
        unregistered: Rc::new(Cell::new(0)),
        component_count: 0,
    };

    block_on(manager.register(&mut router, make_plugin())).unwrap();
    let error = block_on(manager.register(&mut router, make_plugin())).unwrap_err();

    assert!(matches!(error, PluginRegisterError::AlreadyRegistered(found) if found == id));
}

#[derive(Debug, thiserror::Error)]
#[error("plugin registration failed")]
struct RegistrationFailure;

struct FailingPlugin {
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
}

impl Plugin<FRAME_SIZE> for FailingPlugin {
    fn id(&self) -> &'static str {
        "failure"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.load(PendingComponent {
                registered: Rc::clone(&self.registered),
                unregistered: Rc::clone(&self.unregistered),
                storage: context.storage().clone(),
            })?;
            Err(PluginError::registration(RegistrationFailure))
        })
    }
}

#[test]
fn failed_plugin_start_rolls_back_loaded_components() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let id = PluginId::try_from("failure").unwrap();

    block_on(manager.register(
        &mut router,
        FailingPlugin {
            registered: Rc::clone(&registered),
            unregistered: Rc::clone(&unregistered),
        },
    ))
    .unwrap();
    let error = block_on(manager.start(&mut router)).unwrap_err();

    assert!(matches!(error, PluginStartError::Start(_)));
    assert_eq!(registered.get(), 1);
    assert_eq!(unregistered.get(), 1);
    assert!(!manager.is_loaded(&id));
}

type ObservedPair = Rc<RefCell<(Option<Vec<u8>>, Option<Vec<u8>>)>>;

struct AtomicPlugin {
    observed: ObservedPair,
}

impl Plugin<FRAME_SIZE> for AtomicPlugin {
    fn id(&self) -> &'static str {
        "atomic"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context
                .storage()
                .commit(&[
                    StorageMutation::Put {
                        key: b"b",
                        value: b"second",
                    },
                    StorageMutation::Put {
                        key: b"a",
                        value: b"first",
                    },
                ])
                .await?;
            *self.observed.borrow_mut() = (
                context.storage().get(b"a").await?,
                context.storage().get(b"b").await?,
            );
            Ok(())
        })
    }
}

#[test]
fn scoped_storage_preserves_ekv_atomic_batch_writes() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let observed = Rc::new(RefCell::new((None, None)));

    block_on(manager.register(
        &mut router,
        AtomicPlugin {
            observed: Rc::clone(&observed),
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    assert_eq!(
        *observed.borrow(),
        (Some(b"first".to_vec()), Some(b"second".to_vec()))
    );
}

#[derive(Debug, PartialEq, Eq)]
struct TestCapability(u32);

struct CapabilityProvider {
    capability: Rc<TestCapability>,
}

impl Plugin<FRAME_SIZE> for CapabilityProvider {
    fn id(&self) -> &'static str {
        "provider"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.provide(Rc::clone(&self.capability))?;
            Ok(())
        })
    }
}

struct CapabilityConsumer {
    observed: Rc<RefCell<Option<Rc<TestCapability>>>>,
}

impl Plugin<FRAME_SIZE> for CapabilityConsumer {
    const DEPENDS_ON: &'static [&'static str] = &["provider"];

    fn id(&self) -> &'static str {
        "consumer"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            *self.observed.borrow_mut() = Some(context.require::<TestCapability>("provider")?);
            Ok(())
        })
    }
}

#[test]
fn declared_dependency_can_require_a_typed_capability() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let capability = Rc::new(TestCapability(42));
    let observed = Rc::new(RefCell::new(None));

    block_on(manager.register(
        &mut router,
        CapabilityProvider {
            capability: Rc::clone(&capability),
        },
    ))
    .unwrap();
    block_on(manager.register(
        &mut router,
        CapabilityConsumer {
            observed: Rc::clone(&observed),
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    let required = observed.borrow().clone().unwrap();
    assert!(Rc::ptr_eq(&required, &capability));
    assert_eq!(*required, TestCapability(42));
}

#[test]
fn declared_dependency_must_be_registered_before_consumer() {
    let mut manager = PluginManager::new(store());
    let mut router = router();

    let error = block_on(manager.register(
        &mut router,
        CapabilityConsumer {
            observed: Rc::new(RefCell::new(None)),
        },
    ))
    .unwrap_err();

    assert!(
        matches!(error, PluginRegisterError::MissingDependency(id) if id.as_str() == "provider")
    );
}

struct UndeclaredConsumer;

impl Plugin<FRAME_SIZE> for UndeclaredConsumer {
    fn id(&self) -> &'static str {
        "undeclared"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            let _capability = context.require::<TestCapability>("provider")?;
            Ok(())
        })
    }
}

#[test]
fn require_rejects_an_undeclared_dependency() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    block_on(manager.register(
        &mut router,
        CapabilityProvider {
            capability: Rc::new(TestCapability(1)),
        },
    ))
    .unwrap();

    block_on(manager.register(&mut router, UndeclaredConsumer)).unwrap();
    let error = block_on(manager.start(&mut router)).unwrap_err();

    assert!(matches!(
        error,
        PluginStartError::Start(PluginError::Capability(
            CapabilityError::DependencyNotDeclared(id)
        )) if id.as_str() == "provider"
    ));
}

struct MissingCapabilityConsumer;

impl Plugin<FRAME_SIZE> for MissingCapabilityConsumer {
    const DEPENDS_ON: &'static [&'static str] = &["provider"];

    fn id(&self) -> &'static str {
        "consumer"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            let _capability = context.require::<String>("provider")?;
            Ok(())
        })
    }
}

#[test]
fn require_reports_a_capability_the_provider_did_not_publish() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    block_on(manager.register(
        &mut router,
        CapabilityProvider {
            capability: Rc::new(TestCapability(1)),
        },
    ))
    .unwrap();

    block_on(manager.register(&mut router, MissingCapabilityConsumer)).unwrap();
    let error = block_on(manager.start(&mut router)).unwrap_err();

    assert!(matches!(
        error,
        PluginStartError::Start(PluginError::Capability(
            CapabilityError::NotProvided { provider, .. }
        )) if provider.as_str() == "provider"
    ));
}

#[test]
fn provider_cannot_unload_while_a_dependent_is_loaded() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let provider = PluginId::try_from("provider").unwrap();
    let consumer = PluginId::try_from("consumer").unwrap();
    block_on(manager.register(
        &mut router,
        CapabilityProvider {
            capability: Rc::new(TestCapability(1)),
        },
    ))
    .unwrap();
    block_on(manager.register(
        &mut router,
        CapabilityConsumer {
            observed: Rc::new(RefCell::new(None)),
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();

    let error = manager.unload(&mut router, &provider).unwrap_err();
    assert!(matches!(
        error,
        PluginUnloadError::HasDependents { plugin, dependents }
            if plugin == provider && dependents == vec![consumer.clone()]
    ));

    manager.unload(&mut router, &consumer).unwrap();
    manager.unload(&mut router, &provider).unwrap();
}

struct RetainedResource {
    dropped: Rc<Cell<usize>>,
}

impl Drop for RetainedResource {
    fn drop(&mut self) {
        self.dropped.set(self.dropped.get() + 1);
    }
}

struct RetainingPlugin {
    id: &'static str,
    dropped: Rc<Cell<usize>>,
    fail: bool,
}

impl Plugin<FRAME_SIZE> for RetainingPlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.retain(RetainedResource {
                dropped: Rc::clone(&self.dropped),
            });
            if self.fail {
                return Err(PluginError::registration(RegistrationFailure));
            }
            Ok(())
        })
    }
}

#[test]
fn retained_resources_follow_plugin_lifecycle_and_rollback() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let dropped = Rc::new(Cell::new(0));
    let retained = PluginId::try_from("retained").unwrap();

    block_on(manager.register(
        &mut router,
        RetainingPlugin {
            id: "retained",
            dropped: Rc::clone(&dropped),
            fail: false,
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();
    assert_eq!(dropped.get(), 0);
    manager.unload(&mut router, &retained).unwrap();
    assert_eq!(dropped.get(), 1);

    block_on(manager.register(
        &mut router,
        RetainingPlugin {
            id: "failing-retain",
            dropped: Rc::clone(&dropped),
            fail: true,
        },
    ))
    .unwrap();
    let error = block_on(manager.start(&mut router)).unwrap_err();
    assert!(matches!(error, PluginStartError::Start(_)));
    assert_eq!(dropped.get(), 2);
}

struct DuplicateCapabilityProvider;

impl Plugin<FRAME_SIZE> for DuplicateCapabilityProvider {
    fn id(&self) -> &'static str {
        "provider"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.provide(Rc::new(TestCapability(1)))?;
            context.provide(Rc::new(TestCapability(2)))?;
            Ok(())
        })
    }
}

#[test]
fn duplicate_capability_is_rejected_and_rolled_back() {
    let mut manager = PluginManager::new(store());
    let mut router = router();

    block_on(manager.register(&mut router, DuplicateCapabilityProvider)).unwrap();
    let error = block_on(manager.start(&mut router)).unwrap_err();
    assert!(matches!(
        error,
        PluginStartError::Start(PluginError::Capability(
            CapabilityError::AlreadyProvided { .. }
        ))
    ));

    block_on(manager.register(
        &mut router,
        CapabilityProvider {
            capability: Rc::new(TestCapability(3)),
        },
    ))
    .unwrap();
    block_on(manager.start(&mut router)).unwrap();
}

struct RegisterPhasePlugin {
    phases: Rc<RefCell<Vec<&'static str>>>,
}

impl Plugin<FRAME_SIZE> for RegisterPhasePlugin {
    fn id(&self) -> &'static str {
        "register-phase"
    }

    fn register<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginRegisterFuture<'a> {
        Box::pin(async move {
            self.phases.borrow_mut().push("provider.register");
            Ok(())
        })
    }

    fn start<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            self.phases.borrow_mut().push("provider.start");
            Ok(())
        })
    }
}

struct DependentPhasePlugin {
    phases: Rc<RefCell<Vec<&'static str>>>,
}

impl Plugin<FRAME_SIZE> for DependentPhasePlugin {
    const DEPENDS_ON: &'static [&'static str] = &["register-phase"];

    fn id(&self) -> &'static str {
        "dependent-phase"
    }

    fn register<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginRegisterFuture<'a> {
        Box::pin(async move {
            self.phases.borrow_mut().push("consumer.register");
            Ok(())
        })
    }

    fn start<'a>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            self.phases.borrow_mut().push("consumer.start");
            Ok(())
        })
    }
}

#[test]
fn plugins_register_before_any_plugin_starts() {
    let mut manager = PluginManager::new(store());
    let mut router = router();
    let phases = Rc::new(RefCell::new(Vec::new()));

    block_on(manager.register(
        &mut router,
        RegisterPhasePlugin {
            phases: Rc::clone(&phases),
        },
    ))
    .unwrap();
    block_on(manager.register(
        &mut router,
        DependentPhasePlugin {
            phases: Rc::clone(&phases),
        },
    ))
    .unwrap();

    assert_eq!(
        phases.borrow().as_slice(),
        ["provider.register", "consumer.register"]
    );

    block_on(manager.start(&mut router)).unwrap();

    assert_eq!(
        phases.borrow().as_slice(),
        [
            "provider.register",
            "consumer.register",
            "provider.start",
            "consumer.start",
        ]
    );
}
