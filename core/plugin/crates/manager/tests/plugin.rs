//! Plugin lifecycle integration tests against Event Router and `ekv`.

#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext, RpcLaneStorage,
    RunContext, UnregisterContext,
};
use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, MemoryPartition};
use barracuda_plugin_manager::{
    CapabilityError, Plugin, PluginDeclaration, PluginEntryIterator, PluginError, PluginId,
    PluginIdError, PluginManager, PluginReadTransaction, PluginRegisterContext,
    PluginRegisterError, PluginResult, PluginStartContext, PluginStartError, PluginTaskToken,
    PluginUnloadError, PluginWriteTransaction,
};
use futures_lite::future::{block_on, poll_once};

const FRAME_SIZE: usize = 64;

macro_rules! declare_plugin {
    ($plugin:ty, $id:literal) => {
        impl PluginDeclaration for $plugin {
            const ID: &'static str = $id;
        }
    };
    ($plugin:ty, $id:literal, [$($dependency:literal),+ $(,)?]) => {
        impl PluginDeclaration for $plugin {
            const ID: &'static str = $id;
            const DEPENDS_ON: &'static [&'static str] = &[$($dependency),+];
        }
    };
}

fn manager() -> PluginManager<FRAME_SIZE, MemoryPartition> {
    block_on(async {
        let partition = memory_partition(MAX_CAPACITY)
            .await
            .expect("create database partition");
        PluginManager::open(partition)
            .await
            .expect("open Plugin storage")
    })
}

fn router() -> EventRouter<8, FRAME_SIZE, 8> {
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    block_on(EventRouter::new(lanes)).expect("create Event Router")
}

struct PendingComponent<Storage: barracuda_plugin_manager::PluginStorage> {
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
    storage: Storage,
}

impl<Storage: barracuda_plugin_manager::PluginStorage> Component<FRAME_SIZE>
    for PendingComponent<Storage>
{
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

struct StatefulPlugin<const KIND: u8> {
    value: u32,
    observed: Rc<RefCell<Option<u32>>>,
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
    component_count: usize,
}

struct IdentifiedPlugin;

declare_plugin!(IdentifiedPlugin, "identified");

impl Plugin<FRAME_SIZE> for IdentifiedPlugin {}

struct InvalidIdentityPlugin;

declare_plugin!(InvalidIdentityPlugin, "");

impl Plugin<FRAME_SIZE> for InvalidIdentityPlugin {}

#[test]
fn register_uses_the_identity_declared_by_the_plugin() {
    let mut manager = manager();
    let mut router = router();
    let id = PluginId::try_from("identified").unwrap();

    manager.register(&mut router, IdentifiedPlugin).unwrap();

    assert!(manager.is_loaded(&id));
}

impl<const KIND: u8> PluginDeclaration for StatefulPlugin<KIND> {
    const ID: &'static str = match KIND {
        0 => "scheduler",
        1 => "other",
        2 => "multi",
        _ => "duplicate",
    };
}

impl<const KIND: u8> Plugin<FRAME_SIZE> for StatefulPlugin<KIND> {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        block_on(async {
            *self.observed.borrow_mut() = context.storage().get("state").await?;
            context.storage().put("state", &self.value).await?;
            Ok::<(), PluginError>(())
        })?;

        for _ in 0..self.component_count {
            context.event_router.load(PendingComponent {
                registered: Rc::clone(&self.registered),
                unregistered: Rc::clone(&self.unregistered),
                storage: context.storage().clone(),
            })?;
        }
        Ok(())
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
    let mut manager = manager();
    let mut router = router();

    let error = manager
        .register(&mut router, InvalidIdentityPlugin)
        .unwrap_err();

    assert!(matches!(
        error,
        PluginRegisterError::InvalidId(PluginIdError::Empty)
    ));
}

#[test]
fn unloading_an_unknown_plugin_is_rejected() {
    let mut manager = manager();
    let mut router = router();
    let id = PluginId::try_from("unknown").unwrap();

    let error = block_on(manager.unload(&mut router, &id)).unwrap_err();

    assert!(matches!(
        error,
        barracuda_plugin_manager::PluginUnloadError::NotFound(found) if found == id
    ));
}

#[test]
fn plugins_have_isolated_durable_scopes() {
    let mut manager = manager();
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let scheduler_observed = Rc::new(RefCell::new(None));
    let other_observed = Rc::new(RefCell::new(None));

    manager
        .register(
            &mut router,
            StatefulPlugin::<0> {
                value: 41,
                observed: Rc::clone(&scheduler_observed),
                registered: Rc::clone(&registered),
                unregistered: Rc::clone(&unregistered),
                component_count: 1,
            },
        )
        .unwrap();
    manager
        .register(
            &mut router,
            StatefulPlugin::<1> {
                value: 72,
                observed: Rc::clone(&other_observed),
                registered: Rc::clone(&registered),
                unregistered: Rc::clone(&unregistered),
                component_count: 1,
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    assert_eq!(*scheduler_observed.borrow(), None);
    assert_eq!(*other_observed.borrow(), None);

    block_on(manager.unload(&mut router, &PluginId::try_from("scheduler").unwrap())).unwrap();
    let restored = Rc::new(RefCell::new(None));
    manager
        .register(
            &mut router,
            StatefulPlugin::<0> {
                value: 99,
                observed: Rc::clone(&restored),
                registered: Rc::clone(&registered),
                unregistered: Rc::clone(&unregistered),
                component_count: 1,
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    assert_eq!(*restored.borrow(), Some(41));
}

#[test]
fn one_plugin_can_register_multiple_components() {
    let mut manager = manager();
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let id = PluginId::try_from("multi").unwrap();

    manager
        .register(
            &mut router,
            StatefulPlugin::<2> {
                value: 1,
                observed: Rc::new(RefCell::new(None)),
                registered: Rc::clone(&registered),
                unregistered: Rc::clone(&unregistered),
                component_count: 3,
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    assert_eq!(registered.get(), 3);
    assert_eq!(manager.component_ids(&id).unwrap().len(), 3);
    block_on(manager.unload(&mut router, &id)).unwrap();
    assert_eq!(unregistered.get(), 3);
}

#[test]
fn duplicate_plugin_id_is_rejected() {
    let mut manager = manager();
    let mut router = router();
    let id = PluginId::try_from("duplicate").unwrap();
    let make_plugin = || StatefulPlugin::<3> {
        value: 1,
        observed: Rc::new(RefCell::new(None)),
        registered: Rc::new(Cell::new(0)),
        unregistered: Rc::new(Cell::new(0)),
        component_count: 0,
    };

    manager.register(&mut router, make_plugin()).unwrap();
    let error = manager.register(&mut router, make_plugin()).unwrap_err();

    assert!(matches!(error, PluginRegisterError::AlreadyRegistered(found) if found == id));
}

#[derive(Debug, thiserror::Error)]
#[error("plugin registration failed")]
struct RegistrationFailure;

struct FailingPlugin {
    registered: Rc<Cell<usize>>,
    unregistered: Rc<Cell<usize>>,
}

declare_plugin!(FailingPlugin, "failure");

impl Plugin<FRAME_SIZE> for FailingPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.event_router.load(PendingComponent {
            registered: Rc::clone(&self.registered),
            unregistered: Rc::clone(&self.unregistered),
            storage: context.storage().clone(),
        })?;
        Ok(())
    }

    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Err(PluginError::registration(RegistrationFailure))
    }
}

#[test]
fn failed_plugin_start_rolls_back_loaded_components() {
    let mut manager = manager();
    let mut router = router();
    let registered = Rc::new(Cell::new(0));
    let unregistered = Rc::new(Cell::new(0));
    let id = PluginId::try_from("failure").unwrap();

    manager
        .register(
            &mut router,
            FailingPlugin {
                registered: Rc::clone(&registered),
                unregistered: Rc::clone(&unregistered),
            },
        )
        .unwrap();
    let error = manager.start(&mut router).unwrap_err();

    assert!(matches!(error, PluginStartError::Start(_)));
    assert_eq!(registered.get(), 1);
    assert_eq!(unregistered.get(), 1);
    assert!(!manager.is_loaded(&id));
}

struct TaskStartingFailure {
    token: Rc<RefCell<Option<PluginTaskToken>>>,
}

declare_plugin!(TaskStartingFailure, "task-starting-failure");

impl Plugin<FRAME_SIZE> for TaskStartingFailure {
    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.token.replace(Some(context.task_token()));
        Err(PluginError::registration(RegistrationFailure))
    }
}

#[test]
fn failed_plugin_start_cancels_tasks_started_by_that_hook() {
    let mut manager = manager();
    let mut router = router();
    let token = Rc::new(RefCell::new(None));

    manager
        .register(
            &mut router,
            TaskStartingFailure {
                token: Rc::clone(&token),
            },
        )
        .expect("register Plugin");
    let error = manager.start(&mut router).expect_err("startup must fail");

    assert!(matches!(error, PluginStartError::Start(_)));
    assert!(token
        .borrow()
        .as_ref()
        .is_some_and(PluginTaskToken::is_cancelled));
}

struct TaskTokenPlugin {
    token: Rc<RefCell<Option<PluginTaskToken>>>,
}

declare_plugin!(TaskTokenPlugin, "task-token");

impl Plugin<FRAME_SIZE> for TaskTokenPlugin {
    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.token.replace(Some(context.task_token()));
        Ok(())
    }
}

#[test]
fn dropping_the_manager_cancels_its_task_tokens() {
    let mut manager = manager();
    let mut router = router();
    let token = Rc::new(RefCell::new(None));

    manager
        .register(
            &mut router,
            TaskTokenPlugin {
                token: Rc::clone(&token),
            },
        )
        .expect("register Plugin");
    manager.start(&mut router).expect("start Plugin");
    assert!(token
        .borrow()
        .as_ref()
        .is_some_and(|token| !token.is_cancelled()));

    drop(manager);

    assert!(token
        .borrow()
        .as_ref()
        .is_some_and(PluginTaskToken::is_cancelled));
}

#[test]
fn shutdown_waits_for_plugin_task_completion() {
    let mut manager = manager();
    let mut router = router();
    let token = Rc::new(RefCell::new(None));

    manager
        .register(
            &mut router,
            TaskTokenPlugin {
                token: Rc::clone(&token),
            },
        )
        .expect("register Plugin");
    manager.start(&mut router).expect("start Plugin");

    let mut shutdown = Box::pin(manager.shutdown(&mut router));
    assert!(block_on(poll_once(shutdown.as_mut())).is_none());
    assert!(token
        .borrow()
        .as_ref()
        .is_some_and(PluginTaskToken::is_cancelled));

    drop(token.borrow_mut().take());
    block_on(shutdown).expect("shutdown Plugin graph");
}

type ObservedPair = Rc<RefCell<(Option<u32>, Option<u32>)>>;

struct AtomicPlugin {
    observed: ObservedPair,
}

declare_plugin!(AtomicPlugin, "atomic");

impl Plugin<FRAME_SIZE> for AtomicPlugin {
    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        block_on(async {
            let mut transaction = context.storage().write_transaction().await;
            transaction.write("a", &1_u32).await?;
            transaction.write("b", &2_u32).await?;
            transaction.commit().await?;
            *self.observed.borrow_mut() = (
                context.storage().get("a").await?,
                context.storage().get("b").await?,
            );
            Ok::<(), PluginError>(())
        })
    }
}

#[test]
fn scoped_storage_preserves_ekv_write_transactions() {
    let mut manager = manager();
    let mut router = router();
    let observed = Rc::new(RefCell::new((None, None)));

    manager
        .register(
            &mut router,
            AtomicPlugin {
                observed: Rc::clone(&observed),
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    assert_eq!(*observed.borrow(), (Some(1), Some(2)));
}

struct KeyWriterPlugin;

declare_plugin!(KeyWriterPlugin, "key-writer");

impl Plugin<FRAME_SIZE> for KeyWriterPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        block_on(context.storage().put("private", &1_u32))?;
        Ok(())
    }
}

struct LateKeyWriterPlugin;

declare_plugin!(LateKeyWriterPlugin, "zzzzzzzzzzzz");

impl Plugin<FRAME_SIZE> for LateKeyWriterPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        block_on(context.storage().put("also-private", &2_u32))?;
        Ok(())
    }
}

struct EntryIteratorPlugin {
    observed: Rc<RefCell<Vec<(String, u32)>>>,
}

declare_plugin!(EntryIteratorPlugin, "entry-iterator");

impl Plugin<FRAME_SIZE> for EntryIteratorPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        block_on(async {
            context.storage().put("beta", &2_u32).await?;
            context.storage().put("alpha", &1_u32).await?;
            let read = context.storage().read_transaction().await;
            let mut entries = read.entries().await?;
            while let Some(entry) = entries.next().await? {
                self.observed
                    .borrow_mut()
                    .push((String::from(entry.key()), entry.value::<u32>()?));
            }
            Ok::<(), PluginError>(())
        })
    }
}

#[test]
fn plugin_entry_iterator_streams_relative_keys_and_values_only_from_its_namespace() {
    let mut manager = manager();
    let mut router = router();
    let observed = Rc::new(RefCell::new(Vec::new()));

    manager.register(&mut router, KeyWriterPlugin).unwrap();
    manager.register(&mut router, LateKeyWriterPlugin).unwrap();
    manager
        .register(
            &mut router,
            EntryIteratorPlugin {
                observed: Rc::clone(&observed),
            },
        )
        .unwrap();

    assert_eq!(
        observed.borrow().as_slice(),
        [(String::from("alpha"), 1), (String::from("beta"), 2)]
    );
}

#[derive(Debug, PartialEq, Eq)]
struct TestCapability(u32);

struct CapabilityProvider {
    capability: Rc<TestCapability>,
}

declare_plugin!(CapabilityProvider, "provider");

impl Plugin<FRAME_SIZE> for CapabilityProvider {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.provide(Rc::clone(&self.capability))?;
        Ok(())
    }
}

struct CapabilityConsumer {
    observed: Rc<RefCell<Option<Rc<TestCapability>>>>,
}

declare_plugin!(CapabilityConsumer, "consumer", ["provider"]);

impl Plugin<FRAME_SIZE> for CapabilityConsumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        *self.observed.borrow_mut() = Some(context.require::<TestCapability>("provider")?);
        Ok(())
    }
}

#[test]
fn declared_dependency_can_require_a_typed_capability() {
    let mut manager = manager();
    let mut router = router();
    let capability = Rc::new(TestCapability(42));
    let observed = Rc::new(RefCell::new(None));

    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::clone(&capability),
            },
        )
        .unwrap();
    manager
        .register(
            &mut router,
            CapabilityConsumer {
                observed: Rc::clone(&observed),
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    let required = observed.borrow().clone().unwrap();
    assert!(Rc::ptr_eq(&required, &capability));
    assert_eq!(*required, TestCapability(42));
}

#[test]
fn queued_plugins_register_in_dependency_order() {
    let mut manager = manager();
    let mut router = router();
    let capability = Rc::new(TestCapability(42));
    let observed = Rc::new(RefCell::new(None));

    manager
        .add(CapabilityConsumer {
            observed: Rc::clone(&observed),
        })
        .unwrap();
    manager
        .add(CapabilityProvider {
            capability: Rc::clone(&capability),
        })
        .unwrap();
    manager.register_all(&mut router).unwrap();

    let required = observed.borrow().clone().unwrap();
    assert!(Rc::ptr_eq(&required, &capability));
}

struct CycleA;

declare_plugin!(CycleA, "cycle-a", ["cycle-b"]);

impl Plugin<FRAME_SIZE> for CycleA {}

struct CycleB;

declare_plugin!(CycleB, "cycle-b", ["cycle-a"]);

impl Plugin<FRAME_SIZE> for CycleB {}

#[test]
fn queued_plugin_cycle_is_rejected() {
    let mut manager = manager();
    let mut router = router();
    manager.add(CycleB).unwrap();
    manager.add(CycleA).unwrap();

    let error = manager.register_all(&mut router).unwrap_err();

    assert!(matches!(error, PluginRegisterError::DependencyCycle(ids) if ids.len() == 2));
}

#[test]
fn declared_dependency_must_be_registered_before_consumer() {
    let mut manager = manager();
    let mut router = router();

    let error = manager
        .register(
            &mut router,
            CapabilityConsumer {
                observed: Rc::new(RefCell::new(None)),
            },
        )
        .unwrap_err();

    assert!(
        matches!(error, PluginRegisterError::MissingDependency(id) if id.as_str() == "provider")
    );
}

struct UndeclaredConsumer;

declare_plugin!(UndeclaredConsumer, "undeclared");

impl Plugin<FRAME_SIZE> for UndeclaredConsumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let _capability = context.require::<TestCapability>("provider")?;
        Ok(())
    }
}

#[test]
fn require_rejects_an_undeclared_dependency() {
    let mut manager = manager();
    let mut router = router();
    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::new(TestCapability(1)),
            },
        )
        .unwrap();

    let error = manager
        .register(&mut router, UndeclaredConsumer)
        .unwrap_err();

    assert!(matches!(
        error,
        PluginRegisterError::Registration(PluginError::Capability(
            CapabilityError::DependencyNotDeclared(id)
        )) if id.as_str() == "provider"
    ));
}

struct MissingCapabilityConsumer;

declare_plugin!(MissingCapabilityConsumer, "consumer", ["provider"]);

impl Plugin<FRAME_SIZE> for MissingCapabilityConsumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let _capability = context.require::<String>("provider")?;
        Ok(())
    }
}

#[test]
fn require_reports_a_capability_the_provider_did_not_publish() {
    let mut manager = manager();
    let mut router = router();
    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::new(TestCapability(1)),
            },
        )
        .unwrap();

    let error = manager
        .register(&mut router, MissingCapabilityConsumer)
        .unwrap_err();

    assert!(matches!(
        error,
        PluginRegisterError::Registration(PluginError::Capability(
            CapabilityError::NotProvided { provider, .. }
        )) if provider.as_str() == "provider"
    ));
}

#[test]
fn provider_cannot_unload_while_a_dependent_is_loaded() {
    let mut manager = manager();
    let mut router = router();
    let provider = PluginId::try_from("provider").unwrap();
    let consumer = PluginId::try_from("consumer").unwrap();
    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::new(TestCapability(1)),
            },
        )
        .unwrap();
    manager
        .register(
            &mut router,
            CapabilityConsumer {
                observed: Rc::new(RefCell::new(None)),
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    let error = block_on(manager.unload(&mut router, &provider)).unwrap_err();
    assert!(matches!(
        error,
        PluginUnloadError::HasDependents { plugin, dependents }
            if plugin == provider && dependents == vec![consumer.clone()]
    ));

    block_on(manager.unload(&mut router, &consumer)).unwrap();
    block_on(manager.unload(&mut router, &provider)).unwrap();
}

#[test]
fn shutdown_unloads_the_complete_graph_in_reverse_dependency_order() {
    let mut manager = manager();
    let mut router = router();
    let provider = PluginId::try_from("provider").unwrap();
    let consumer = PluginId::try_from("consumer").unwrap();
    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::new(TestCapability(1)),
            },
        )
        .unwrap();
    manager
        .register(
            &mut router,
            CapabilityConsumer {
                observed: Rc::new(RefCell::new(None)),
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();

    block_on(manager.shutdown(&mut router)).unwrap();

    assert!(!manager.is_loaded(&provider));
    assert!(!manager.is_loaded(&consumer));
}

struct RetainedResource {
    dropped: Rc<Cell<usize>>,
}

impl Drop for RetainedResource {
    fn drop(&mut self) {
        self.dropped.set(self.dropped.get() + 1);
    }
}

struct RetainingPlugin<const FAILING: bool> {
    dropped: Rc<Cell<usize>>,
    fail: bool,
}

impl<const FAILING: bool> PluginDeclaration for RetainingPlugin<FAILING> {
    const ID: &'static str = if FAILING {
        "failing-retain"
    } else {
        "retained"
    };
}

impl<const FAILING: bool> Plugin<FRAME_SIZE> for RetainingPlugin<FAILING> {
    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.retain(RetainedResource {
            dropped: Rc::clone(&self.dropped),
        });
        if self.fail {
            return Err(PluginError::registration(RegistrationFailure));
        }
        Ok(())
    }
}

#[test]
fn retained_resources_follow_plugin_lifecycle_and_rollback() {
    let mut manager = manager();
    let mut router = router();
    let dropped = Rc::new(Cell::new(0));
    let retained = PluginId::try_from("retained").unwrap();

    manager
        .register(
            &mut router,
            RetainingPlugin::<false> {
                dropped: Rc::clone(&dropped),
                fail: false,
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();
    assert_eq!(dropped.get(), 0);
    block_on(manager.unload(&mut router, &retained)).unwrap();
    assert_eq!(dropped.get(), 1);

    manager
        .register(
            &mut router,
            RetainingPlugin::<true> {
                dropped: Rc::clone(&dropped),
                fail: true,
            },
        )
        .unwrap();
    let error = manager.start(&mut router).unwrap_err();
    assert!(matches!(error, PluginStartError::Start(_)));
    assert_eq!(dropped.get(), 2);
}

struct DuplicateCapabilityProvider;

declare_plugin!(DuplicateCapabilityProvider, "provider");

impl Plugin<FRAME_SIZE> for DuplicateCapabilityProvider {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.provide(Rc::new(TestCapability(1)))?;
        context.provide(Rc::new(TestCapability(2)))?;
        Ok(())
    }
}

#[test]
fn duplicate_capability_is_rejected_and_rolled_back() {
    let mut manager = manager();
    let mut router = router();

    let error = manager
        .register(&mut router, DuplicateCapabilityProvider)
        .unwrap_err();
    assert!(matches!(
        error,
        PluginRegisterError::Registration(PluginError::Capability(
            CapabilityError::AlreadyProvided { .. }
        ))
    ));

    manager
        .register(
            &mut router,
            CapabilityProvider {
                capability: Rc::new(TestCapability(3)),
            },
        )
        .unwrap();
    manager.start(&mut router).unwrap();
}

struct RegisterPhasePlugin {
    phases: Rc<RefCell<Vec<&'static str>>>,
}

declare_plugin!(RegisterPhasePlugin, "register-phase");

impl Plugin<FRAME_SIZE> for RegisterPhasePlugin {
    fn register<Storage>(
        &mut self,
        _context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.phases.borrow_mut().push("provider.register");
        Ok(())
    }

    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.phases.borrow_mut().push("provider.start");
        Ok(())
    }
}

struct DependentPhasePlugin {
    phases: Rc<RefCell<Vec<&'static str>>>,
}

declare_plugin!(DependentPhasePlugin, "dependent-phase", ["register-phase"]);

impl Plugin<FRAME_SIZE> for DependentPhasePlugin {
    fn register<Storage>(
        &mut self,
        _context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.phases.borrow_mut().push("consumer.register");
        Ok(())
    }

    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        self.phases.borrow_mut().push("consumer.start");
        Ok(())
    }
}

#[test]
fn plugins_register_before_any_plugin_starts() {
    let mut manager = manager();
    let mut router = router();
    let phases = Rc::new(RefCell::new(Vec::new()));

    manager
        .register(
            &mut router,
            RegisterPhasePlugin {
                phases: Rc::clone(&phases),
            },
        )
        .unwrap();
    manager
        .register(
            &mut router,
            DependentPhasePlugin {
                phases: Rc::clone(&phases),
            },
        )
        .unwrap();

    assert_eq!(
        phases.borrow().as_slice(),
        ["provider.register", "consumer.register"]
    );

    manager.start(&mut router).unwrap();

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
