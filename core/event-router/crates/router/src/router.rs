//! Low-level Component router runtime.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use getset::CopyGetters;

use super::component::{
    Component, ComponentError, ComponentFuture, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_rpc::{RpcError, RpcLaneStorage, RpcRegistration, RpcRegistry, RpcRegistryApi};

const FIRST_COMPONENT_ID: u64 = 1;

/// Stable identity assigned to one loaded Component instance.
#[derive(Clone, Copy, CopyGetters, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId {
    /// Numeric identity value.
    #[getset(get_copy = "pub")]
    value: u64,
}

impl ComponentId {
    const fn from_raw(value: u64) -> Self {
        Self { value }
    }
}

/// Failure while loading a Component.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The Event Router Future has already terminated.
    #[error("Event Router has terminated")]
    RouterTerminated,
    /// No unused Component identity remains.
    #[error("Component identity space is exhausted")]
    IdentifierExhausted,
    /// The Component could not register its interfaces or local resources.
    #[error("Component registration failed: {0}")]
    Component(#[source] ComponentError),
    /// Registration failed and rolling back the partial registration also failed.
    #[error("Component registration failed and rollback was incomplete")]
    Rollback {
        /// Original registration failure.
        #[source]
        source: ComponentError,
        /// Cleanup failures encountered during rollback.
        cleanup: Box<CleanupError>,
    },
}

/// Failure while unloading a Component.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UnloadError {
    /// No loaded Component has this identity.
    #[error("Component is not loaded: {0}")]
    NotFound(ComponentId),
    /// Component teardown or RPC cleanup failed.
    #[error(transparent)]
    Cleanup(#[from] CleanupError),
}

/// Failures collected while tearing down one Component.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CleanupError {
    /// The Component's synchronous teardown failed.
    #[error("Component teardown failed: {0}")]
    Component(#[source] ComponentError),
    /// One or more RPC registrations could not be removed.
    #[error("RPC registration cleanup failed: {0:?}")]
    Rpc(Vec<RpcError>),
    /// Both Component teardown and RPC cleanup failed.
    #[error("Component teardown and RPC registration cleanup failed: {rpc:?}")]
    ComponentAndRpc {
        /// Component teardown failure.
        #[source]
        component: ComponentError,
        /// RPC cleanup failures.
        rpc: Vec<RpcError>,
    },
}

/// Cleanup failure associated with one Component during Router termination.
#[derive(Debug)]
#[non_exhaustive]
pub struct ComponentCleanupFailure {
    /// Component whose teardown was incomplete.
    pub id: ComponentId,
    /// Teardown failure for that Component.
    pub error: CleanupError,
}

/// Terminal failure produced while the Router is driven as a Future.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RouterError {
    /// A Component's long-lived run future ended successfully without being unloaded.
    #[error("Component exited while still loaded: {id}")]
    ComponentExited {
        /// Component that exited.
        id: ComponentId,
    },
    /// A Component's long-lived run future failed.
    #[error("Component failed while running: {id}: {source}")]
    ComponentFailed {
        /// Component that failed.
        id: ComponentId,
        /// Component lifecycle failure.
        #[source]
        source: ComponentError,
    },
    /// Router termination also encountered Component cleanup failures.
    #[error("Event Router terminated and {} Component cleanup operation(s) failed", failures.len())]
    CleanupFailed {
        /// Original reason the Router terminated.
        #[source]
        cause: Box<RouterError>,
        /// Component teardown failures collected before returning.
        failures: Vec<ComponentCleanupFailure>,
    },
    /// The Event Router Future was polled after it had terminated.
    #[error("Event Router was polled after termination")]
    PolledAfterTermination,
}

impl core::fmt::Display for ComponentId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "component-{}", self.value)
    }
}

type SharedComponent<const M: usize> = Rc<RefCell<Box<dyn Component<M>>>>;

struct ComponentEntry<const M: usize> {
    component: SharedComponent<M>,
    run: Option<ComponentFuture<'static>>,
    registrations: Vec<RpcRegistration>,
}

/// Cooperative Component router backed by one task-local RPC registry.
///
/// The Router creates no task or executor. An external executor advances
/// loaded Components by polling this value as a [`Future`]. [`load`](Self::load)
/// and [`unload`](Self::unload) mutate the loaded set between polls. Unload is
/// hard cancellation: Component run futures must be cancellation-safe.
pub struct Router<const N: usize, const M: usize, const Q: usize> {
    registry: RpcRegistry<N, M, Q>,
    components: BTreeMap<ComponentId, ComponentEntry<M>>,
    next_component_id: Option<u64>,
    terminated: bool,
}

impl<const N: usize, const M: usize, const Q: usize> Router<N, M, Q> {
    /// Creates an empty Router backed by application-lifetime RPC lanes.
    #[must_use]
    pub fn new(lanes: &'static RpcLaneStorage<N, M, Q>) -> Self {
        log::info!("creating Event Router");
        Self {
            registry: RpcRegistry::new(lanes),
            components: BTreeMap::new(),
            next_component_id: Some(FIRST_COMPONENT_ID),
            terminated: false,
        }
    }

    /// Registers and loads one Component without starting an executor.
    ///
    /// The Component begins making progress the next time this Router is
    /// polled. A failed registration is rolled back before this method returns.
    ///
    /// # Errors
    ///
    /// Returns an error when Component identity allocation, registration, or
    /// rollback fails.
    pub fn load(&mut self, mut component: Box<dyn Component<M>>) -> Result<ComponentId, LoadError> {
        if self.terminated {
            log::warn!("refusing Component load after Event Router termination");
            return Err(LoadError::RouterTerminated);
        }
        let id = self.allocate_component_id()?;
        log::debug!("registering Component {id}");
        let registry: &dyn RpcRegistryApi<M> = &self.registry;
        let mut registrations = Vec::new();
        if let Err(source) =
            component.register(&mut RegisterContext::new(registry, &mut registrations))
        {
            log::error!("Component {id} registration failed: {source}");
            let component_error = component.unregister(&mut UnregisterContext::new()).err();
            let rpc_errors = unregister_all(registry, registrations);
            return match cleanup_error(component_error, rpc_errors) {
                None => Err(LoadError::Component(source)),
                Some(cleanup) => Err(LoadError::Rollback {
                    source,
                    cleanup: Box::new(cleanup),
                }),
            };
        }

        let component = Rc::new(RefCell::new(component));
        let run = component_run(Rc::clone(&component), registry.client());
        self.components.insert(
            id,
            ComponentEntry {
                component,
                run: Some(run),
                registrations,
            },
        );
        log::info!("loaded Component {id}");
        Ok(id)
    }

    /// Cancels, unregisters, and removes one loaded Component.
    ///
    /// Dropping the Component's run future first releases its mutable lifecycle
    /// borrow. Synchronous Component teardown runs before its RPC registrations
    /// are removed.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is unknown or teardown is incomplete. When
    /// teardown fails, the Component identity remains loaded in a cleanup-only
    /// state so the caller can retry `unload`.
    pub fn unload(&mut self, id: ComponentId) -> Result<(), UnloadError> {
        let Some(mut entry) = self.components.remove(&id) else {
            log::warn!("cannot unload unknown Component {id}");
            return Err(UnloadError::NotFound(id));
        };
        log::info!("unloading Component {id}");
        drop(entry.run.take());
        let registry: &dyn RpcRegistryApi<M> = &self.registry;
        let (remaining, rpc_errors) = revoke_all(registry, entry.registrations);
        entry.registrations = remaining;
        let component_error = if entry.registrations.is_empty() {
            entry
                .component
                .borrow_mut()
                .unregister(&mut UnregisterContext::new())
                .err()
        } else {
            None
        };
        match cleanup_error(component_error, rpc_errors) {
            Some(error) => {
                log::error!("Component {id} unload failed: {error}");
                self.components.insert(id, entry);
                Err(UnloadError::Cleanup(error))
            }
            None => {
                log::info!("unloaded Component {id}");
                Ok(())
            }
        }
    }

    fn allocate_component_id(&mut self) -> Result<ComponentId, LoadError> {
        let value = self
            .next_component_id
            .ok_or(LoadError::IdentifierExhausted)?;
        self.next_component_id = value.checked_add(1);
        Ok(ComponentId::from_raw(value))
    }

    fn cleanup_all(&mut self) -> Vec<ComponentCleanupFailure> {
        let mut entries: Vec<_> = core::mem::take(&mut self.components)
            .into_iter()
            .map(|(id, entry)| (id, entry, Vec::new()))
            .collect();
        let registry: &dyn RpcRegistryApi<M> = &self.registry;

        for (_id, entry, rpc_errors) in &mut entries {
            let registrations = core::mem::take(&mut entry.registrations);
            let (_remaining, errors) = revoke_all(registry, registrations);
            *rpc_errors = errors;
        }
        for (_id, entry, _rpc_errors) in &mut entries {
            drop(entry.run.take());
        }

        let mut failures = Vec::new();
        for (id, entry, rpc_errors) in entries {
            let component_error = entry
                .component
                .borrow_mut()
                .unregister(&mut UnregisterContext::new())
                .err();
            if let Some(error) = cleanup_error(component_error, rpc_errors) {
                failures.push(ComponentCleanupFailure { id, error });
            }
        }
        failures
    }
}

impl<const N: usize, const M: usize, const Q: usize> Drop for Router<N, M, Q> {
    fn drop(&mut self) {
        let failures = self.cleanup_all();
        for failure in failures {
            log::error!(
                "Component {} cleanup failed while dropping Event Router: {}",
                failure.id,
                failure.error
            );
        }
    }
}

impl<const N: usize, const M: usize, const Q: usize> Future for Router<N, M, Q> {
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(Err(RouterError::PolledAfterTermination));
        }
        let mut completed = None;
        for (id, entry) in &mut this.components {
            let outcome = entry
                .run
                .as_mut()
                .and_then(|run| match run.as_mut().poll(context) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => None,
                });
            if let Some(result) = outcome {
                completed = Some((*id, result));
                break;
            }
        }
        let Some((id, result)) = completed else {
            return Poll::Pending;
        };
        this.terminated = true;
        let cause = match result {
            Ok(()) => RouterError::ComponentExited { id },
            Err(source) => RouterError::ComponentFailed { id, source },
        };
        let failures = this.cleanup_all();
        if failures.is_empty() {
            log::error!("Event Router terminated: {cause}");
            Poll::Ready(Err(cause))
        } else {
            log::error!(
                "Event Router terminated with {} cleanup failure(s): {cause}",
                failures.len()
            );
            Poll::Ready(Err(RouterError::CleanupFailed {
                cause: Box::new(cause),
                failures,
            }))
        }
    }
}

fn component_run<const M: usize>(
    component: SharedComponent<M>,
    rpc: barracuda_rpc::RpcClient,
) -> ComponentFuture<'static> {
    // The guard deliberately owns the Component for its complete run future.
    // Router drops that future before unload borrows the Component again,
    // and the task-local runtime never sends or concurrently polls this guard.
    #[allow(clippy::await_holding_refcell_ref)]
    Box::pin(async move {
        let mut component = component.borrow_mut();
        component.run(RunContext::new(rpc)).await
    })
}

fn unregister_all<const M: usize>(
    registry: &dyn RpcRegistryApi<M>,
    registrations: Vec<RpcRegistration>,
) -> Vec<RpcError> {
    let mut errors = Vec::new();
    for registration in registrations.iter().rev() {
        if let Err(error) = registry.unregister(registration) {
            errors.push(error);
        }
    }
    errors
}

fn revoke_all<const M: usize>(
    registry: &dyn RpcRegistryApi<M>,
    registrations: Vec<RpcRegistration>,
) -> (Vec<RpcRegistration>, Vec<RpcError>) {
    let mut remaining = Vec::new();
    let mut errors = Vec::new();
    for registration in registrations.into_iter().rev() {
        if let Err(error) = registry.revoke(&registration) {
            remaining.push(registration);
            errors.push(error);
        }
    }
    remaining.reverse();
    (remaining, errors)
}

fn cleanup_error(component: Option<ComponentError>, rpc: Vec<RpcError>) -> Option<CleanupError> {
    match (component, rpc.is_empty()) {
        (None, true) => None,
        (Some(component), true) => Some(CleanupError::Component(component)),
        (None, false) => Some(CleanupError::Rpc(rpc)),
        (Some(component), false) => Some(CleanupError::ComponentAndRpc { component, rpc }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    extern crate std;

    use alloc::boxed::Box;
    use core::future::{pending, poll_fn, Future};
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::task::Wake;

    use futures_lite::future::block_on;
    use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

    use super::{LoadError, Router, RouterError, UnloadError};
    use crate::{
        Component, ComponentError, ComponentFuture, RegisterContext, RunContext, UnregisterContext,
    };
    use barracuda_rpc::{RpcError, RpcFrame, RpcLaneStorage, RpcMethod, Unary};

    const FRAME_SIZE: usize = 64;

    fn router() -> Router<4, FRAME_SIZE, 4> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
        Router::new(lanes)
    }

    fn poll_router(router: &mut Router<4, FRAME_SIZE, 4>) -> Poll<Result<(), RouterError>> {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        Pin::new(router).poll(&mut context)
    }

    #[derive(Default)]
    struct ProbeState {
        registered: core::cell::Cell<bool>,
        polls: core::cell::Cell<usize>,
        unregistered: core::cell::Cell<bool>,
    }

    struct PendingComponent {
        state: Rc<ProbeState>,
    }

    impl Component<FRAME_SIZE> for PendingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            self.state.registered.set(true);
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(poll_fn(move |_context| {
                self.state
                    .polls
                    .set(self.state.polls.get().saturating_add(1));
                Poll::Pending
            }))
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            self.state.unregistered.set(true);
            Ok(())
        }
    }

    #[test]
    fn load_poll_and_unload_drive_one_component_lifecycle() {
        let mut router = router();
        let state = Rc::new(ProbeState::default());

        let component = router
            .load(Box::new(PendingComponent {
                state: Rc::clone(&state),
            }))
            .expect("load component");

        assert!(state.registered.get());
        assert!(matches!(poll_router(&mut router), Poll::Pending));
        assert_eq!(state.polls.get(), 1);

        router.unload(component).expect("unload component");
        assert!(state.unregistered.get());
        assert!(matches!(poll_router(&mut router), Poll::Pending));
        assert_eq!(state.polls.get(), 1);
    }

    #[test]
    fn loading_between_polls_preserves_existing_component_run_state() {
        let mut router = router();
        let first = Rc::new(ProbeState::default());
        let second = Rc::new(ProbeState::default());
        let first_id = router
            .load(Box::new(PendingComponent {
                state: Rc::clone(&first),
            }))
            .expect("load first component");

        assert!(matches!(poll_router(&mut router), Poll::Pending));
        assert_eq!(first.polls.get(), 1);

        let second_id = router
            .load(Box::new(PendingComponent {
                state: Rc::clone(&second),
            }))
            .expect("load second component between polls");
        assert!(matches!(poll_router(&mut router), Poll::Pending));
        assert_eq!(first.polls.get(), 2);
        assert_eq!(second.polls.get(), 1);

        router.unload(first_id).expect("unload first component");
        router.unload(second_id).expect("unload second component");
    }

    #[repr(C)]
    #[derive(
        Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes,
    )]
    struct Number(u32);

    struct Increment;

    impl RpcMethod for Increment {
        const ADDRESS: &'static str = "event-router.increment";
        type Request = Number;
        type Response = Number;
        type Error = ();
        type Input = Unary;
        type Output = Unary;
    }

    struct RpcComponent;

    impl Component<FRAME_SIZE> for RpcComponent {
        fn register(
            &mut self,
            context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            context.register_rpc::<Increment, _>(
                "system",
                |_context, request: RpcFrame<Number>| async move {
                    Ok(Ok(Number(request.view()?.0.saturating_add(1))))
                },
            )
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }
    }

    #[test]
    fn unload_removes_component_rpc_endpoints() {
        let mut router = router();
        let component = router
            .load(Box::new(RpcComponent))
            .expect("load RPC component");
        let client = router.registry.client();

        let response = block_on(
            client
                .call::<Increment>(Number(9))
                .expect("prepare component RPC"),
        )
        .expect("drive component RPC")
        .expect("component method success");
        assert_eq!(response.view().expect("view response"), &Number(10));
        drop(response);

        router.unload(component).expect("unload RPC component");
        assert!(matches!(
            client.call::<Increment>(Number(9)),
            Err(RpcError::NotFound(_))
        ));
    }

    struct HangingRpcComponent {
        handler_polls: Rc<core::cell::Cell<usize>>,
        unregistered: Rc<core::cell::Cell<bool>>,
    }

    impl Component<FRAME_SIZE> for HangingRpcComponent {
        fn register(
            &mut self,
            context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            let handler_polls = Rc::clone(&self.handler_polls);
            context.register_rpc::<Increment, _>("system", move |_context, _request| {
                let handler_polls = Rc::clone(&handler_polls);
                poll_fn(move |_context| {
                    handler_polls.set(handler_polls.get().saturating_add(1));
                    Poll::<Result<Result<Number, ()>, RpcError>>::Pending
                })
            })
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            self.unregistered.set(true);
            Ok(())
        }
    }

    #[test]
    fn unload_revokes_an_in_flight_component_rpc_before_teardown() {
        let mut router = router();
        let handler_polls = Rc::new(core::cell::Cell::new(0));
        let unregistered = Rc::new(core::cell::Cell::new(false));
        let component = router
            .load(Box::new(HangingRpcComponent {
                handler_polls: Rc::clone(&handler_polls),
                unregistered: Rc::clone(&unregistered),
            }))
            .expect("load hanging RPC component");
        let mut call = Box::pin(
            router
                .registry
                .client()
                .call::<Increment>(Number(9))
                .expect("prepare hanging RPC"),
        );
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);

        assert!(matches!(call.as_mut().poll(&mut context), Poll::Pending));
        let polls_before_unload = handler_polls.get();
        assert_ne!(polls_before_unload, 0);

        router.unload(component).expect("unload RPC component");
        assert!(unregistered.get());
        assert_eq!(handler_polls.get(), polls_before_unload);
        assert!(matches!(
            call.as_mut().poll(&mut context),
            Poll::Ready(Err(RpcError::EndpointRevoked(_)))
        ));
        assert_eq!(handler_polls.get(), polls_before_unload);
    }

    struct DuplicateRpcComponent {
        unregistered: Rc<core::cell::Cell<bool>>,
    }

    impl Component<FRAME_SIZE> for DuplicateRpcComponent {
        fn register(
            &mut self,
            context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            context.register_rpc::<Increment, _>(
                "system",
                |_context, request: RpcFrame<Number>| async move { Ok(Ok(*request.view()?)) },
            )?;
            context.register_rpc::<Increment, _>(
                "system",
                |_context, request: RpcFrame<Number>| async move { Ok(Ok(*request.view()?)) },
            )
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            self.unregistered.set(true);
            Ok(())
        }
    }

    #[test]
    fn failed_load_rolls_back_registrations_and_component_resources() {
        let mut router = router();
        let unregistered = Rc::new(core::cell::Cell::new(false));

        let error = router
            .load(Box::new(DuplicateRpcComponent {
                unregistered: Rc::clone(&unregistered),
            }))
            .expect_err("duplicate RPC must fail load");

        assert!(matches!(error, LoadError::Component(_)));
        assert!(unregistered.get());
        assert!(matches!(
            router.registry.client().call::<Increment>(Number(1)),
            Err(RpcError::NotFound(_))
        ));
    }

    #[test]
    fn unloading_unknown_component_returns_not_found() {
        let mut router = router();
        let missing = super::ComponentId::from_raw(42);

        assert!(matches!(
            router.unload(missing),
            Err(UnloadError::NotFound(id)) if id == missing
        ));
    }

    #[derive(Debug, thiserror::Error)]
    #[error("component cleanup must be retried")]
    struct CleanupMustBeRetried;

    struct RetryCleanupComponent {
        attempts: Rc<core::cell::Cell<usize>>,
    }

    impl Component<FRAME_SIZE> for RetryCleanupComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            let attempt = self.attempts.get().saturating_add(1);
            self.attempts.set(attempt);
            if attempt == 1 {
                Err(ComponentError::lifecycle(CleanupMustBeRetried))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn failed_unload_retains_the_component_for_cleanup_retry() {
        let mut router = router();
        let attempts = Rc::new(core::cell::Cell::new(0));
        let component = router
            .load(Box::new(RetryCleanupComponent {
                attempts: Rc::clone(&attempts),
            }))
            .expect("load retry cleanup component");

        assert!(matches!(
            router.unload(component),
            Err(UnloadError::Cleanup(_))
        ));
        assert_eq!(attempts.get(), 1);

        router.unload(component).expect("retry component cleanup");
        assert_eq!(attempts.get(), 2);
        assert!(matches!(
            router.unload(component),
            Err(UnloadError::NotFound(id)) if id == component
        ));
    }

    #[test]
    fn dropping_router_unregisters_loaded_components() {
        let state = Rc::new(ProbeState::default());
        let mut router = router();
        router
            .load(Box::new(PendingComponent {
                state: Rc::clone(&state),
            }))
            .expect("load component");
        assert!(matches!(poll_router(&mut router), Poll::Pending));

        drop(router);

        assert!(state.unregistered.get());
    }

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct SelfWakingComponent;

    impl Component<FRAME_SIZE> for SelfWakingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(poll_fn(|context| {
                context.waker().wake_by_ref();
                Poll::Pending
            }))
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }
    }

    #[test]
    fn component_wake_is_forwarded_without_lifecycle_self_wakes() {
        let mut router = router();
        let component = router
            .load(Box::new(SelfWakingComponent))
            .expect("load component");
        let wakes = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wakes));
        let mut context = Context::from_waker(&waker);
        assert!(matches!(
            Pin::new(&mut router).poll(&mut context),
            Poll::Pending
        ));
        assert_eq!(wakes.0.load(Ordering::Relaxed), 1);

        router.unload(component).expect("unload component");
        assert_eq!(wakes.0.load(Ordering::Relaxed), 1);
    }

    #[derive(Debug, thiserror::Error)]
    #[error("component run failed")]
    struct RunFailed;

    struct FailingComponent;

    impl Component<FRAME_SIZE> for FailingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(async { Err(ComponentError::lifecycle(RunFailed)) })
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }
    }

    #[test]
    fn component_run_failure_terminates_router_with_component_identity() {
        let mut router = router();
        let component = router
            .load(Box::new(FailingComponent))
            .expect("load failing component");

        assert!(matches!(
            poll_router(&mut router),
            Poll::Ready(Err(RouterError::ComponentFailed { id, .. })) if id == component
        ));
        assert!(matches!(
            router.unload(component),
            Err(UnloadError::NotFound(id)) if id == component
        ));
    }

    struct CompletingComponent {
        state: Rc<ProbeState>,
    }

    impl Component<FRAME_SIZE> for CompletingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            self.state.unregistered.set(true);
            Ok(())
        }
    }

    #[test]
    fn component_exit_cleans_up_every_loaded_component_before_router_terminates() {
        let mut router = router();
        let completing_state = Rc::new(ProbeState::default());
        let pending_state = Rc::new(ProbeState::default());
        let component = router
            .load(Box::new(CompletingComponent {
                state: Rc::clone(&completing_state),
            }))
            .expect("load completing component");
        router
            .load(Box::new(PendingComponent {
                state: Rc::clone(&pending_state),
            }))
            .expect("load pending component");

        assert!(matches!(
            poll_router(&mut router),
            Poll::Ready(Err(RouterError::ComponentExited { id })) if id == component
        ));
        assert!(completing_state.unregistered.get());
        assert!(pending_state.unregistered.get());
        assert!(matches!(
            router.load(Box::new(PendingComponent {
                state: Rc::new(ProbeState::default()),
            })),
            Err(LoadError::RouterTerminated)
        ));
        assert!(matches!(
            router.unload(component),
            Err(UnloadError::NotFound(id)) if id == component
        ));
    }

    struct CompletingWithFailedCleanup;

    impl Component<FRAME_SIZE> for CompletingWithFailedCleanup {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            Err(ComponentError::lifecycle(CleanupMustBeRetried))
        }
    }

    #[test]
    fn router_termination_reports_component_cleanup_failures() {
        let mut router = router();
        let component = router
            .load(Box::new(CompletingWithFailedCleanup))
            .expect("load component with failing cleanup");

        assert!(matches!(
            poll_router(&mut router),
            Poll::Ready(Err(RouterError::CleanupFailed { cause, failures }))
                if matches!(*cause, RouterError::ComponentExited { id } if id == component)
                    && failures.len() == 1
                    && failures.first().is_some_and(|failure| failure.id == component)
        ));
    }

    struct HandlerDropSignal {
        dropped: Rc<core::cell::Cell<bool>>,
    }

    impl Drop for HandlerDropSignal {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("component cleanup ran before its in-flight handler was dropped")]
    struct HandlerStillAlive;

    struct TeardownOrderRpcComponent {
        handler_dropped: Rc<core::cell::Cell<bool>>,
    }

    impl Component<FRAME_SIZE> for TeardownOrderRpcComponent {
        fn register(
            &mut self,
            context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            let handler_dropped = Rc::clone(&self.handler_dropped);
            context.register_rpc::<Increment, _>("system", move |_context, _request| {
                let signal = HandlerDropSignal {
                    dropped: Rc::clone(&handler_dropped),
                };
                async move {
                    let _signal = signal;
                    pending::<Result<Result<Number, ()>, RpcError>>().await
                }
            })
        }

        fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(pending())
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            if self.handler_dropped.get() {
                Ok(())
            } else {
                Err(ComponentError::lifecycle(HandlerStillAlive))
            }
        }
    }

    struct RpcCallingComponent;

    impl Component<FRAME_SIZE> for RpcCallingComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, FRAME_SIZE>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
            Box::pin(async move {
                let _outcome = context.rpc().call::<Increment>(Number(1))?.await?;
                Ok(())
            })
        }

        fn unregister(
            &mut self,
            _context: &mut UnregisterContext<'_>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }
    }

    #[test]
    fn router_termination_drops_all_run_futures_before_component_cleanup() {
        let mut router = router();
        let handler_dropped = Rc::new(core::cell::Cell::new(false));
        router
            .load(Box::new(TeardownOrderRpcComponent {
                handler_dropped: Rc::clone(&handler_dropped),
            }))
            .expect("load RPC provider");
        router
            .load(Box::new(RpcCallingComponent))
            .expect("load RPC caller");
        let completing = router
            .load(Box::new(CompletingComponent {
                state: Rc::new(ProbeState::default()),
            }))
            .expect("load completing component");

        assert!(matches!(
            poll_router(&mut router),
            Poll::Ready(Err(RouterError::ComponentExited { id })) if id == completing
        ));
        assert!(handler_dropped.get());
    }
}
