//! Component lifecycle interface owned by Event Router.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::error::Error;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;

use barracuda_rpc::{RpcClient, RpcError, RpcHandler, RpcMethod, RpcRegistration, RpcRegistryApi};
use getset::Getters;

/// Result returned by Component lifecycle operations.
pub type ComponentResult<T> = Result<T, ComponentError>;

/// Cooperative future returned by [`Component::run`].
pub type ComponentFuture<'a> = Pin<Box<dyn Future<Output = ComponentResult<()>> + 'a>>;

/// Error returned by a Component lifecycle operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ComponentError {
    /// RPC registration or invocation failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// The Component's own lifecycle logic failed.
    #[error("Component lifecycle failed: {0}")]
    Lifecycle(#[source] Box<dyn Error>),
}

impl ComponentError {
    /// Wraps a Component-specific lifecycle error.
    #[must_use]
    pub fn lifecycle(error: impl Error + 'static) -> Self {
        Self::Lifecycle(Box::new(error))
    }
}

/// Context provided while a Component registers its RPC endpoints.
pub struct RegisterContext<'a, const M: usize> {
    registry: &'a dyn RpcRegistryApi<M>,
    registrations: &'a mut Vec<RpcRegistration>,
}

impl<'a, const M: usize> RegisterContext<'a, M> {
    pub(crate) fn new(
        registry: &'a dyn RpcRegistryApi<M>,
        registrations: &'a mut Vec<RpcRegistration>,
    ) -> Self {
        Self {
            registry,
            registrations,
        }
    }

    /// Registers one typed RPC handler owned by the Component.
    ///
    /// The registration is recorded so Event Router can remove it
    /// automatically when registration is rolled back or the Component is
    /// unloaded.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the method is invalid, its address is already
    /// occupied, or registration otherwise fails.
    ///
    /// # Compile-time layout checks
    ///
    /// A method whose fixed-layout message exceeds `M` does not compile:
    ///
    /// ```compile_fail
    /// use barracuda_router::{ComponentResult, RegisterContext};
    /// use barracuda_rpc::{RpcFrame, RpcMethod, Unary};
    ///
    /// struct TooLarge;
    ///
    /// impl RpcMethod for TooLarge {
    ///     const ADDRESS: &'static str = "component.too_large";
    ///     type Request = [u8; 65];
    ///     type Response = [u8; 1];
    ///     type Error = [u8; 1];
    ///     type Input = Unary;
    ///     type Output = Unary;
    /// }
    ///
    /// fn register(context: &mut RegisterContext<'_, 64>) -> ComponentResult<()> {
    ///     context.register_rpc::<TooLarge, _>(
    ///         |_context, _request: RpcFrame<[u8; 65]>| async move { Ok(Ok([0])) },
    ///     )
    /// }
    ///
    /// fn context() -> &'static mut RegisterContext<'static, 64> {
    ///     loop {}
    /// }
    ///
    /// fn main() {
    ///     let _ = register(context());
    /// }
    /// ```
    pub fn register_rpc<Method, H>(&mut self, handler: H) -> ComponentResult<()>
    where
        Method: RpcMethod,
        H: RpcHandler<Method> + 'static,
    {
        let registration = self.registry.register_rpc::<Method, H>(handler)?;
        self.registrations.push(registration);
        Ok(())
    }
}

/// Context owned by a Component for the duration of [`Component::run`].
#[derive(Getters)]
pub struct RunContext<const M: usize> {
    /// Client used to call RPC endpoints.
    #[getset(get = "pub")]
    rpc: RpcClient,
}

impl<const M: usize> RunContext<M> {
    pub(crate) const fn new(rpc: RpcClient) -> Self {
        Self { rpc }
    }
}

/// Context provided while a Component performs synchronous teardown.
pub struct UnregisterContext<'a> {
    marker: PhantomData<&'a mut ()>,
}

impl UnregisterContext<'_> {
    pub(crate) const fn new() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

/// Interface implemented by one Event Router runtime Component.
///
/// `M` is the RPC lane frame capacity. Keeping it in the interface lets
/// [`RegisterContext::register_rpc`] reject oversized method frames at compile
/// time while the lane count and waiter capacity remain hidden.
pub trait Component<const M: usize> {
    /// Registers the Component's interfaces and initializes local resources.
    ///
    /// # Errors
    ///
    /// Returns an error when registration or synchronous initialization fails.
    /// Event Router rolls back recorded registrations and calls
    /// [`unregister`](Self::unregister) so the Component can release resources
    /// created before the failure.
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()>;

    /// Returns the Component's single cooperative running future.
    ///
    /// Event Router may drop this future at any suspension point during
    /// synchronous unload or Router termination. Implementations must therefore
    /// be cancellation-safe: dropping the future must not leave borrowed or
    /// externally visible state inconsistent. The drop releases the mutable
    /// Component borrow before [`unregister`](Self::unregister) is called.
    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a>;

    /// Performs synchronous teardown after the running future has ended or a
    /// partial [`register`](Self::register) operation has failed.
    ///
    /// Event Router owns removal of every RPC registration recorded during
    /// [`register`](Self::register); Components must not remove those endpoints
    /// themselves.
    /// When this method returns an error during explicit unload, Event Router
    /// retains the Component and may call `unregister` again when the caller
    /// retries unloading the same Component identity. Implementations must make
    /// failed cleanup attempts retry-safe.
    ///
    /// # Errors
    ///
    /// Returns an error when synchronous teardown fails.
    fn unregister(&mut self, context: &mut UnregisterContext<'_>) -> ComponentResult<()>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::future::pending;

    use futures_lite::future::block_on;
    use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

    use super::{
        Component, ComponentError, ComponentFuture, RegisterContext, RunContext, UnregisterContext,
    };
    use barracuda_rpc::{
        RpcError, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcRegistryApi, Unary,
    };

    #[repr(C)]
    #[derive(
        Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes,
    )]
    struct Number(u32);

    struct Increment;

    #[derive(Debug, thiserror::Error)]
    #[error("increment RPC returned a method error")]
    struct IncrementFailed;

    impl RpcMethod for Increment {
        const ADDRESS: &'static str = "component.increment";
        type Request = Number;
        type Response = Number;
        type Error = Number;
        type Input = Unary;
        type Output = Unary;
    }

    struct IncrementComponent {
        last_response: Option<Number>,
    }

    impl Component<256> for IncrementComponent {
        fn register(
            &mut self,
            context: &mut RegisterContext<'_, 256>,
        ) -> Result<(), ComponentError> {
            context.register_rpc::<Increment, _>(|_context, request: RpcFrame<Number>| async move {
                Ok(Ok(Number(request.view()?.0.saturating_add(1))))
            })
        }

        fn run<'a>(&'a mut self, context: RunContext<256>) -> ComponentFuture<'a> {
            Box::pin(async move {
                let response = context
                    .rpc()
                    .call::<Increment>(Number(41))?
                    .await?
                    .map_err(|_| ComponentError::lifecycle(IncrementFailed))?;
                self.last_response = Some(*response.view()?);
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

    fn registry<const N: usize, const M: usize, const Q: usize>() -> RpcRegistry<N, M, Q> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
        RpcRegistry::new(lanes)
    }

    #[test]
    fn contexts_retain_only_frame_capacity_from_the_registry_type() {
        let registry = registry::<2, 256, 2>();
        let registry_api: &dyn RpcRegistryApi<256> = &registry;
        let mut registrations = Vec::new();
        let mut component = IncrementComponent {
            last_response: None,
        };

        let component_api: &mut dyn Component<256> = &mut component;
        component_api
            .register(&mut RegisterContext::new(registry_api, &mut registrations))
            .expect("register component");
        block_on(component_api.run(RunContext::new(registry_api.client()))).expect("run component");

        component_api
            .unregister(&mut UnregisterContext::new())
            .expect("unregister component");
        assert_eq!(component.last_response, Some(Number(42)));
        for registration in registrations.drain(..).rev() {
            registry_api
                .unregister(&registration)
                .expect("remove component RPC");
        }
        assert!(matches!(
            registry.client().call::<Increment>(Number(1)),
            Err(RpcError::NotFound(_))
        ));
    }

    struct PassiveComponent;

    impl Component<64> for PassiveComponent {
        fn register(
            &mut self,
            _context: &mut RegisterContext<'_, 64>,
        ) -> Result<(), ComponentError> {
            Ok(())
        }

        fn run<'a>(&'a mut self, _context: RunContext<64>) -> ComponentFuture<'a> {
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
    fn dropping_run_future_releases_component_for_unregister() {
        let registry = registry::<1, 64, 1>();
        let registry_api: &dyn RpcRegistryApi<64> = &registry;
        let mut component = PassiveComponent;
        let run = component.run(RunContext::new(registry_api.client()));

        drop(run);
        component
            .unregister(&mut UnregisterContext::new())
            .expect("unregister after run cancellation");
    }
}
