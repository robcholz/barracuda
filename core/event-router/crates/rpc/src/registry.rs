use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::any::type_name;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::marker::PhantomData;
use core::mem::size_of;
use core::pin::Pin;

use getset::{CopyGetters, Getters};
use smallvec::{smallvec, SmallVec};

use super::address::{RpcAddress, RpcAddressError, RpcGroup};
use super::context::{RpcCallId, RpcContext, RpcEndpointId};
use super::json::{
    JsonCall, JsonHandler, JsonHandlerAdapter, JsonPayload, JsonRpcInfo, JsonRpcSchema,
};
use super::lane::{LaneAcquireSet, LaneIoSet, LanePool, LaneReader, LaneWriter, RpcLaneStorage};
use super::payload::{RpcMulticastBranch, RpcPayloadReader, RpcPayloadWriter};
use super::typed::{
    HandlerAdapter, RpcCardinality, RpcHandler, RpcInputMode, RpcMessage, RpcMethod,
    RpcMethodDescriptor, RpcOutputMode,
};

/// Request or response side of one full-duplex RPC lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RpcDirection {
    /// Data sent from caller to handler.
    Request,
    /// Data sent from handler to caller.
    Response,
}

/// Result returned by RPC operations.
pub type RpcResult<T> = Result<T, RpcError>;

pub(crate) type RpcFuture<'a> = Pin<Box<dyn Future<Output = RpcResult<()>> + 'a>>;

pub(crate) trait ErasedRpcHandler {
    fn call<'a>(
        &'a self,
        context: RpcContext,
        input: LaneReader,
        output: LaneWriter,
    ) -> RpcFuture<'a>;
}

/// Opaque, type-erased RPC endpoint accepted by [`RpcRegistryApi`].
///
/// Components create endpoints through their registration context; callers do
/// not need access to the erased handler or wire descriptor stored inside.
pub struct RpcEndpoint<const M: usize> {
    address: RpcAddress,
    visibility: String,
    handler: Rc<dyn ErasedRpcHandler>,
    contract: EndpointContract,
}

impl<const M: usize> RpcEndpoint<M> {
    pub(crate) fn typed<Method, H>(visibility: &str, handler: H) -> RpcResult<Self>
    where
        Method: RpcMethod,
        H: RpcHandler<Method> + 'static,
    {
        const {
            assert!(
                size_of::<Method::Request>() <= M,
                "RPC request message exceeds lane frame capacity"
            );
            assert!(
                size_of::<Method::Response>() <= M,
                "RPC response message exceeds lane frame capacity"
            );
            assert!(
                size_of::<Method::Error>() <= M,
                "RPC method error exceeds lane frame capacity"
            );
        }
        let descriptor = RpcMethodDescriptor::for_method::<Method>()?;
        Ok(Self {
            address: descriptor.address().clone(),
            visibility: visibility.into(),
            handler: Rc::new(HandlerAdapter::<Method, H>::new(handler)),
            contract: EndpointContract::Typed(descriptor),
        })
    }

    pub(crate) fn json<Method, H>(visibility: &str, handler: H) -> RpcResult<Self>
    where
        Method: JsonRpcSchema,
        H: JsonHandler + 'static,
    {
        const {
            assert!(
                Method::MAX_REQUEST_BYTES > 0,
                "JSON RPC request capacity must be nonzero"
            );
            assert!(
                Method::MAX_REQUEST_BYTES <= M,
                "JSON RPC request exceeds lane frame capacity"
            );
            assert!(
                Method::MAX_RESPONSE_BYTES > 0,
                "JSON RPC response capacity must be nonzero"
            );
            assert!(
                Method::MAX_RESPONSE_BYTES <= M,
                "JSON RPC response exceeds lane frame capacity"
            );
        }
        let info = JsonRpcInfo::for_method::<Method>()?;
        Ok(Self {
            address: info.address().clone(),
            visibility: visibility.into(),
            handler: Rc::new(JsonHandlerAdapter::<Method, H>::new(handler)),
            contract: EndpointContract::Json(info),
        })
    }
}

#[derive(Clone)]
enum EndpointContract {
    Typed(RpcMethodDescriptor),
    Json(JsonRpcInfo),
}

impl EndpointContract {
    fn request_frame_capacity(&self) -> usize {
        match self {
            Self::Typed(descriptor) => descriptor.request_frame_size(),
            Self::Json(info) => info.max_request_bytes(),
        }
    }
}

/// Task-local registry that resolves typed RPC addresses.
///
/// The registry internally shares its task-local core with clients and does
/// not require handlers or futures to be `Send`. It is intended to run inside
/// Event Router's cooperative executor thread. Root calls wait when every lane
/// is active; nested calls fail instead of waiting when doing so could deadlock.
pub struct RpcRegistry<const N: usize, const M: usize, const Q: usize> {
    core: Rc<RegistryCore>,
    storage: PhantomData<&'static RpcLaneStorage<N, M, Q>>,
}

/// Object-safe registry interface used by runtime components.
///
/// `M` remains part of the interface so typed endpoint frames are checked at
/// compile time. The concrete registry's lane count `N` and waiter capacity
/// `Q` do not propagate into components.
pub trait RpcRegistryApi<const M: usize> {
    /// Creates an external client for this registry.
    fn client(&self) -> RpcClient;

    /// Registers one previously type-erased RPC endpoint.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the address is occupied or identifiers are
    /// exhausted.
    fn register_endpoint(&self, endpoint: RpcEndpoint<M>) -> RpcResult<RpcRegistration>;

    /// Unregisters the endpoint instance represented by `registration`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::StaleRegistration`] when the token no longer owns
    /// the registered address.
    fn unregister(&self, registration: &RpcRegistration) -> RpcResult<()>;

    /// Removes an owned endpoint and prevents prepared calls from polling its
    /// handler again.
    fn revoke(&self, registration: &RpcRegistration) -> RpcResult<()>;
}

impl<const M: usize> dyn RpcRegistryApi<M> + '_ {
    /// Registers a typed RPC while preserving compile-time frame-size checks.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the method cannot be registered.
    pub fn register_rpc<Method, H>(
        &self,
        visibility: &str,
        handler: H,
    ) -> RpcResult<RpcRegistration>
    where
        Method: RpcMethod,
        H: RpcHandler<Method> + 'static,
    {
        self.register_endpoint(RpcEndpoint::<M>::typed::<Method, H>(visibility, handler)?)
    }

    /// Registers a lane-native JSON RPC handler.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the method address is invalid or occupied.
    pub fn register_json_rpc<Method, H>(
        &self,
        visibility: &str,
        handler: H,
    ) -> RpcResult<RpcRegistration>
    where
        Method: JsonRpcSchema,
        H: JsonHandler + 'static,
    {
        self.register_endpoint(RpcEndpoint::<M>::json::<Method, H>(visibility, handler)?)
    }
}

struct RegistryCore {
    endpoints: RefCell<BTreeMap<RpcAddress, EndpointEntry>>,
    next_endpoint_id: Cell<u64>,
    next_call_id: Cell<u64>,
    lanes: &'static dyn LanePool,
}

impl<const N: usize, const M: usize, const Q: usize> RpcRegistry<N, M, Q> {
    /// Creates an empty registry backed by fixed-capacity lane storage.
    ///
    /// `N` and `M` must both be nonzero; invalid const-generic configurations
    /// fail at compile time.
    #[must_use]
    pub fn new(lanes: &'static RpcLaneStorage<N, M, Q>) -> Self {
        const {
            assert!(N > 0, "RPC lane count must be nonzero");
            assert!(M > 0, "RPC lane frame capacity must be nonzero");
        }
        Self {
            core: Rc::new(RegistryCore {
                endpoints: RefCell::new(BTreeMap::new()),
                next_endpoint_id: Cell::new(1),
                next_call_id: Cell::new(1),
                lanes,
            }),
            storage: PhantomData,
        }
    }

    /// Creates an external client for this registry.
    ///
    /// Calls from the returned client start new root call chains.
    #[must_use]
    pub fn client(&self) -> RpcClient {
        RpcClient {
            registry: Rc::downgrade(&self.core),
            caller_endpoint_id: None,
            parent_call_id: None,
            root_call_id: None,
        }
    }

    /// Returns a sorted snapshot of groups that contain RPCs.
    ///
    /// Each group appears once. Registering or unregistering an RPC does not
    /// mutate a previously returned snapshot; call this method again to observe
    /// the new registry state.
    #[must_use]
    pub fn groups(&self) -> Vec<RpcGroup> {
        let endpoints = self.core.endpoints.borrow();
        let mut groups = Vec::new();
        for address in endpoints.keys() {
            push_group(&mut groups, address);
        }
        groups.sort_unstable();
        groups
    }

    /// Returns a sorted snapshot of RPC addresses in `group`.
    ///
    /// An unknown group produces an empty snapshot. Registering or
    /// unregistering an RPC does not mutate a previously returned snapshot.
    #[must_use]
    pub fn rpcs(&self, group: &RpcGroup) -> Vec<RpcAddress> {
        let mut addresses: Vec<_> = self
            .core
            .endpoints
            .borrow()
            .keys()
            .filter(|address| address.group() == group.as_ref())
            .cloned()
            .collect();
        addresses.sort_unstable();
        addresses
    }

    /// Returns a sorted snapshot of RPC addresses with `visibility`.
    ///
    /// Visibility is independent of the address-derived [`RpcGroup`]. An
    /// unknown visibility produces an empty snapshot. Registering or
    /// unregistering an RPC does not mutate a previously returned snapshot.
    #[must_use]
    pub fn rpcs_by_visibility(&self, visibility: &str) -> Vec<RpcAddress> {
        self.core.rpcs_by_visibility(visibility)
    }

    /// Registers a handler for method `M`.
    ///
    /// The method descriptor is retained with the endpoint so typed clients can
    /// reject request, response, or cardinality mismatches before payload IO.
    ///
    /// # Compile-time layout checks
    ///
    /// A Method whose fixed-layout message exceeds `M` does not compile:
    ///
    /// ```compile_fail
    /// use barracuda_rpc::{
    ///     RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
    /// };
    /// use static_cell::ConstStaticCell;
    ///
    /// struct TooLargeError;
    ///
    /// impl RpcMethod for TooLargeError {
    ///     const ADDRESS: &'static str = "static.too_large";
    ///     type Request = [u8; 1];
    ///     type Response = [u8; 1];
    ///     type Error = [u8; 65];
    ///     type Input = Unary;
    ///     type Output = Unary;
    /// }
    ///
    /// static LANES: ConstStaticCell<RpcLaneStorage<1, 64, 1>> =
    ///     ConstStaticCell::new(RpcLaneStorage::new());
    /// let registry = RpcRegistry::new(LANES.take());
    /// let _ = registry.register::<TooLargeError, _>(
    ///     "system",
    ///     |_context, _request: RpcFrame<[u8; 1]>| async move { Ok(Ok([0])) },
    /// )?;
    /// # Ok::<(), barracuda_rpc::RpcError>(())
    /// ```
    ///
    /// A Method whose message requires stricter alignment than the lane frame
    /// also does not compile:
    ///
    /// ```compile_fail
    /// use barracuda_rpc::{
    ///     RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
    /// };
    /// use static_cell::ConstStaticCell;
    /// use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};
    ///
    /// #[repr(C, align(32))]
    /// #[derive(Immutable, IntoBytes, KnownLayout, TryFromBytes)]
    /// struct TooAligned([u8; 32]);
    ///
    /// struct InvalidAlignment;
    ///
    /// impl RpcMethod for InvalidAlignment {
    ///     const ADDRESS: &'static str = "static.invalid_alignment";
    ///     type Request = [u8; 1];
    ///     type Response = [u8; 1];
    ///     type Error = TooAligned;
    ///     type Input = Unary;
    ///     type Output = Unary;
    /// }
    ///
    /// static LANES: ConstStaticCell<RpcLaneStorage<1, 64, 1>> =
    ///     ConstStaticCell::new(RpcLaneStorage::new());
    /// let registry = RpcRegistry::new(LANES.take());
    /// let _ = registry.register::<InvalidAlignment, _>(
    ///     "system",
    ///     |_context, _request: RpcFrame<[u8; 1]>| async move { Ok(Ok([0])) },
    /// )?;
    /// # Ok::<(), barracuda_rpc::RpcError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the method address is invalid, the address is
    /// occupied, or no endpoint identity remains.
    ///
    /// Compilation fails if the fixed request, response, or method-error type is
    /// larger than the registry's `M`-byte lane frames or requires stricter
    /// alignment than the lane frame provides.
    pub fn register<Method, H>(&self, visibility: &str, handler: H) -> RpcResult<RpcRegistration>
    where
        Method: RpcMethod,
        H: RpcHandler<Method> + 'static,
    {
        let registry: &dyn RpcRegistryApi<M> = self;
        registry.register_rpc::<Method, H>(visibility, handler)
    }

    /// Registers a lane-native JSON handler for `Method`.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is invalid, occupied, or endpoint
    /// identities are exhausted.
    pub fn register_json<Method, H>(
        &self,
        visibility: &str,
        handler: H,
    ) -> RpcResult<RpcRegistration>
    where
        Method: JsonRpcSchema,
        H: JsonHandler + 'static,
    {
        let registry: &dyn RpcRegistryApi<M> = self;
        registry.register_json_rpc::<Method, H>(visibility, handler)
    }

    /// Unregisters the exact endpoint instance represented by `registration`.
    ///
    /// Calls already in flight retain their handler and may finish normally.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::StaleRegistration`] when the address is absent or
    /// now belongs to a different endpoint instance.
    pub fn unregister(&self, registration: &RpcRegistration) -> RpcResult<()> {
        let mut endpoints = self.core.endpoints.borrow_mut();
        let is_current = endpoints
            .get(&registration.address)
            .is_some_and(|entry| entry.endpoint_id == registration.endpoint_id);
        if !is_current {
            return Err(RpcError::StaleRegistration(registration.address.clone()));
        }
        endpoints.remove(&registration.address);
        Ok(())
    }

    pub(crate) fn revoke(&self, registration: &RpcRegistration) -> RpcResult<()> {
        let endpoint = {
            let mut endpoints = self.core.endpoints.borrow_mut();
            let is_current = endpoints
                .get(&registration.address)
                .is_some_and(|entry| entry.endpoint_id == registration.endpoint_id);
            if !is_current {
                return Err(RpcError::StaleRegistration(registration.address.clone()));
            }
            endpoints
                .remove(&registration.address)
                .ok_or_else(|| RpcError::StaleRegistration(registration.address.clone()))?
        };
        endpoint.lifecycle.revoked.set(true);
        Ok(())
    }
}

impl<const N: usize, const M: usize, const Q: usize> RpcRegistryApi<M> for RpcRegistry<N, M, Q> {
    fn client(&self) -> RpcClient {
        RpcRegistry::client(self)
    }

    fn register_endpoint(&self, endpoint: RpcEndpoint<M>) -> RpcResult<RpcRegistration> {
        self.core.insert_handler(
            endpoint.address,
            endpoint.visibility,
            endpoint.handler,
            endpoint.contract,
        )
    }

    fn unregister(&self, registration: &RpcRegistration) -> RpcResult<()> {
        RpcRegistry::unregister(self, registration)
    }

    fn revoke(&self, registration: &RpcRegistration) -> RpcResult<()> {
        RpcRegistry::revoke(self, registration)
    }
}

impl RegistryCore {
    fn rpcs_by_visibility(&self, visibility: &str) -> Vec<RpcAddress> {
        let mut addresses: Vec<_> = self
            .endpoints
            .borrow()
            .iter()
            .filter(|(_address, endpoint)| endpoint.visibility == visibility)
            .map(|(address, _endpoint)| address.clone())
            .collect();
        addresses.sort_unstable();
        addresses
    }

    fn insert_handler(
        &self,
        address: RpcAddress,
        visibility: String,
        handler: Rc<dyn ErasedRpcHandler>,
        contract: EndpointContract,
    ) -> RpcResult<RpcRegistration> {
        if self.endpoints.borrow().contains_key(&address) {
            return Err(RpcError::AlreadyRegistered(address));
        }
        let endpoint_id = RpcEndpointId::new(self.take_endpoint_id()?);
        let lifecycle = Rc::new(EndpointLifecycle {
            address: address.clone(),
            revoked: Cell::new(false),
        });
        let registration = RpcRegistration {
            address: address.clone(),
            endpoint_id,
        };
        self.endpoints.borrow_mut().insert(
            address,
            EndpointEntry {
                endpoint_id,
                visibility,
                handler,
                contract,
                lifecycle,
            },
        );
        Ok(registration)
    }

    fn method_info(&self, address: &RpcAddress) -> RpcResult<RpcMethodInfo> {
        let entry = self
            .endpoints
            .borrow()
            .get(address)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(address.clone()))?;
        match entry.contract {
            EndpointContract::Typed(descriptor) => Ok(RpcMethodInfo { descriptor }),
            EndpointContract::Json(_) => Err(RpcError::NotTypedEndpoint(address.clone())),
        }
    }

    fn json_method_info(&self, address: &RpcAddress) -> RpcResult<JsonRpcInfo> {
        let entry = self
            .endpoints
            .borrow()
            .get(address)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(address.clone()))?;
        match entry.contract {
            EndpointContract::Typed(_) => Err(RpcError::NotJsonEndpoint(address.clone())),
            EndpointContract::Json(info) => Ok(info),
        }
    }

    fn prepare_typed_call<M>(self: &Rc<Self>, caller: &RpcClient) -> RpcResult<PreparedCalls>
    where
        M: RpcMethod,
    {
        RpcMethodDescriptor::validate_method::<M>()?;
        let endpoint = self.endpoints.borrow().get(M::ADDRESS).cloned();
        let Some(endpoint) = endpoint else {
            return Err(RpcError::NotFound(RpcAddress::try_from(M::ADDRESS)?));
        };
        let EndpointContract::Typed(descriptor) = &endpoint.contract else {
            return Err(RpcError::NotTypedEndpoint(
                endpoint.lifecycle.address.clone(),
            ));
        };
        if !descriptor.is_method::<M>() {
            return Err(RpcError::SignatureMismatch {
                address: descriptor.address().clone(),
                expected: type_name::<M>(),
                registered: descriptor.method_type_name(),
            });
        }
        self.prepare_resolved_call(caller, endpoint)
    }

    fn prepare_json_call(
        self: &Rc<Self>,
        caller: &RpcClient,
        address: &RpcAddress,
    ) -> RpcResult<PreparedCalls> {
        let endpoint = self
            .endpoints
            .borrow()
            .get(address)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(address.clone()))?;
        if !matches!(endpoint.contract, EndpointContract::Json(_)) {
            return Err(RpcError::NotJsonEndpoint(address.clone()));
        }
        self.prepare_resolved_call(caller, endpoint)
    }

    fn prepare_payload_call(
        self: &Rc<Self>,
        caller: &RpcClient,
        address: &RpcAddress,
    ) -> RpcResult<PreparedCalls> {
        let endpoint = self
            .endpoints
            .borrow()
            .get(address)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(address.clone()))?;
        self.prepare_resolved_call(caller, endpoint)
    }

    fn prepare_resolved_call(
        self: &Rc<Self>,
        caller: &RpcClient,
        endpoint: EndpointEntry,
    ) -> RpcResult<PreparedCalls> {
        if caller.caller_endpoint_id == Some(endpoint.endpoint_id) {
            return Err(RpcError::DirectSelfCall(endpoint.lifecycle.address.clone()));
        }

        let call_id = RpcCallId::new(self.take_call_id()?);
        let root_call_id = caller.root_call_id.unwrap_or(call_id);
        let nested_client = RpcClient {
            registry: Rc::downgrade(self),
            caller_endpoint_id: Some(endpoint.endpoint_id),
            parent_call_id: Some(call_id),
            root_call_id: Some(root_call_id),
        };
        let context = RpcContext::new(
            call_id,
            root_call_id,
            caller.parent_call_id,
            caller.caller_endpoint_id,
            endpoint.endpoint_id,
            nested_client,
        );
        Ok(PreparedCalls {
            request_frame_capacity: endpoint.contract.request_frame_capacity(),
            targets: smallvec![PreparedTarget { endpoint, context }],
            lanes: LaneAcquireSet::new(self.lanes, caller.caller_endpoint_id.is_some(), 1),
        })
    }

    fn prepare_multicast_payload(
        self: &Rc<Self>,
        caller: &RpcClient,
        addresses: &[RpcAddress],
    ) -> RpcResult<PreparedCalls> {
        let Some(first_address) = addresses.first() else {
            return Err(RpcError::EmptyMulticast);
        };
        let endpoints = self.endpoints.borrow();
        let first = endpoints
            .get(first_address)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(first_address.clone()))?;
        let EndpointContract::Typed(input_descriptor) = &first.contract else {
            return Err(RpcError::NotTypedEndpoint(first_address.clone()));
        };
        let input_descriptor = input_descriptor.clone();
        let mut resolved = SmallVec::<[_; 1]>::with_capacity(addresses.len());
        for address in addresses {
            let endpoint = endpoints
                .get(address)
                .cloned()
                .ok_or_else(|| RpcError::NotFound(address.clone()))?;
            let EndpointContract::Typed(descriptor) = &endpoint.contract else {
                return Err(RpcError::NotTypedEndpoint(address.clone()));
            };
            if !input_descriptor.has_same_input(descriptor) {
                return Err(RpcError::MulticastInputMismatch {
                    expected: first_address.clone(),
                    actual: address.clone(),
                });
            }
            resolved.push((address.clone(), endpoint));
        }
        drop(endpoints);

        let mut targets = SmallVec::with_capacity(resolved.len());
        let mut multicast_root = caller.root_call_id;
        for (address, endpoint) in resolved {
            if caller.caller_endpoint_id == Some(endpoint.endpoint_id) {
                return Err(RpcError::DirectSelfCall(address));
            }
            let call_id = RpcCallId::new(self.take_call_id()?);
            let root_call_id = multicast_root.unwrap_or(call_id);
            multicast_root = Some(root_call_id);
            let nested_client = RpcClient {
                registry: Rc::downgrade(self),
                caller_endpoint_id: Some(endpoint.endpoint_id),
                parent_call_id: Some(call_id),
                root_call_id: Some(root_call_id),
            };
            let context = RpcContext::new(
                call_id,
                root_call_id,
                caller.parent_call_id,
                caller.caller_endpoint_id,
                endpoint.endpoint_id,
                nested_client,
            );
            targets.push(PreparedTarget { endpoint, context });
        }
        Ok(PreparedCalls {
            request_frame_capacity: input_descriptor.request_frame_size(),
            lanes: LaneAcquireSet::new(
                self.lanes,
                caller.caller_endpoint_id.is_some(),
                targets.len(),
            ),
            targets,
        })
    }

    fn prepare_typed_multicast<T, Mode>(
        self: &Rc<Self>,
        caller: &RpcClient,
        addresses: &[RpcAddress],
    ) -> RpcResult<PreparedCalls>
    where
        T: RpcMessage,
        Mode: RpcInputMode<T>,
    {
        let endpoints = self.endpoints.borrow();
        for address in addresses {
            let endpoint = endpoints
                .get(address)
                .ok_or_else(|| RpcError::NotFound(address.clone()))?;
            let EndpointContract::Typed(descriptor) = &endpoint.contract else {
                return Err(RpcError::NotTypedEndpoint(address.clone()));
            };
            if !descriptor.accepts_input::<T, Mode>() {
                return Err(RpcError::MulticastCallerInputMismatch {
                    address: address.clone(),
                    expected_request: type_name::<T>(),
                    expected_mode: type_name::<Mode>(),
                    registered_request: descriptor.request_type_name(),
                    registered_mode: descriptor.input_mode_type_name(),
                });
            }
        }
        drop(endpoints);
        self.prepare_multicast_payload(caller, addresses)
    }

    fn take_endpoint_id(&self) -> RpcResult<u64> {
        take_identifier(&self.next_endpoint_id)
    }

    fn take_call_id(&self) -> RpcResult<u64> {
        take_identifier(&self.next_call_id)
    }
}

fn push_group(groups: &mut Vec<RpcGroup>, address: &RpcAddress) {
    let group = address.group();
    if !groups.iter().any(|registered| registered.as_ref() == group) {
        groups.push(RpcGroup::from_validated(group));
    }
}

fn take_identifier(next: &Cell<u64>) -> RpcResult<u64> {
    let value = next.get();
    let following = value.checked_add(1).ok_or(RpcError::IdentifierExhausted)?;
    next.set(following);
    Ok(value)
}

#[derive(Clone)]
struct EndpointEntry {
    endpoint_id: RpcEndpointId,
    visibility: String,
    handler: Rc<dyn ErasedRpcHandler>,
    contract: EndpointContract,
    lifecycle: Rc<EndpointLifecycle>,
}

/// Read-only projection of one registered method's signature.
///
/// Returned by [`RpcClient::method_info`]. It exposes type identity and frame
/// size for signature checks.
#[derive(Clone, Getters)]
pub struct RpcMethodInfo {
    /// Read-only view of the registered method's signature descriptor.
    #[getset(get = "pub")]
    descriptor: RpcMethodDescriptor,
}

impl RpcMethodInfo {
    /// Returns this method's fixed request frame size in bytes.
    #[must_use]
    pub fn request_frame_size(&self) -> usize {
        self.descriptor.request_frame_size()
    }

    /// Returns this method's request cardinality.
    #[must_use]
    pub fn input_mode(&self) -> RpcCardinality {
        RpcCardinality::from_type_id(self.descriptor.input_mode_type_id())
    }

    /// Returns this method's response cardinality.
    #[must_use]
    pub fn output_mode(&self) -> RpcCardinality {
        RpcCardinality::from_type_id(self.descriptor.output_mode_type_id())
    }
}

struct EndpointLifecycle {
    address: RpcAddress,
    revoked: Cell<bool>,
}

impl EndpointLifecycle {
    fn error(&self) -> RpcError {
        RpcError::EndpointRevoked(self.address.clone())
    }
}

#[derive(CopyGetters)]
pub(crate) struct PreparedCalls {
    #[getset(get_copy = "pub(crate)")]
    request_frame_capacity: usize,
    targets: SmallVec<[PreparedTarget; 1]>,
    lanes: LaneAcquireSet,
}

impl PreparedCalls {
    pub(crate) fn poll_acquire(
        &mut self,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<RpcResult<LaneIoSet>> {
        if let Some(target) = self
            .targets
            .iter()
            .find(|target| target.endpoint.lifecycle.revoked.get())
        {
            return core::task::Poll::Ready(Err(target.endpoint.lifecycle.error()));
        }
        Pin::new(&mut self.lanes).poll(context)
    }

    pub(crate) fn into_targets(self) -> SmallVec<[PreparedTarget; 1]> {
        self.targets
    }
}

pub(crate) struct PreparedTarget {
    endpoint: EndpointEntry,
    context: RpcContext,
}

impl PreparedTarget {
    pub(crate) fn start(self, input: LaneReader, output: LaneWriter) -> RpcFuture<'static> {
        let endpoint = self.endpoint;
        let context = self.context;
        Box::pin(async move {
            let lifecycle = Rc::clone(&endpoint.lifecycle);
            let mut handler = endpoint.handler.call(context, input, output);
            core::future::poll_fn(move |context| {
                if lifecycle.revoked.get() {
                    core::task::Poll::Ready(Err(lifecycle.error()))
                } else {
                    handler.as_mut().poll(context)
                }
            })
            .await
        })
    }
}

/// Handle used to invoke endpoints in one registry.
#[derive(Clone)]
pub struct RpcClient {
    registry: Weak<RegistryCore>,
    caller_endpoint_id: Option<RpcEndpointId>,
    parent_call_id: Option<RpcCallId>,
    root_call_id: Option<RpcCallId>,
}

impl RpcClient {
    /// Returns a sorted snapshot of RPC addresses with `visibility`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::RegistryDropped`] when the registry is gone.
    pub fn rpcs_by_visibility(&self, visibility: &str) -> RpcResult<Vec<RpcAddress>> {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        Ok(registry.rpcs_by_visibility(visibility))
    }

    /// Starts a typed call for method `M`.
    ///
    /// The one method selects its input and output shape through `M`; callers do
    /// not choose between separate unary and streaming entry points. The
    /// returned future or stream drives request encoding, handler execution,
    /// and response decoding together. Transport/runtime failures use the outer
    /// [`RpcResult`]; successful transport yields the Method's typed
    /// `Result<Response, Error>` as zero-copy frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry was dropped, the endpoint does not
    /// exist, its typed signature differs, identifiers are exhausted, or this
    /// is a direct synchronous self-call. The returned future or stream may
    /// later report lane acquisition or framing errors.
    pub fn call<M>(
        &self,
        input: <M::Input as RpcInputMode<M::Request>>::ClientInput,
    ) -> RpcResult<<M::Output as RpcOutputMode<M::Response, M::Error>>::ClientCall>
    where
        M: RpcMethod,
    {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        let prepared = registry.prepare_typed_call::<M>(self)?;
        Ok(super::typed::make_typed_call::<M>(prepared, input))
    }

    /// Starts a lane-native JSON call at `address`.
    ///
    /// The raw document is validated without building a `serde_json::Value`.
    /// Polling the returned call copies it directly into the request lane and
    /// returns a [`JsonRef`] that retains the response lane.
    ///
    /// # Errors
    ///
    /// Returns an error when the JSON is invalid or too large, the endpoint is
    /// absent or not a JSON endpoint, or call preparation fails.
    pub fn call_json<'a, J>(&self, address: &RpcAddress, request: &'a J) -> RpcResult<JsonCall<'a>>
    where
        J: JsonPayload + ?Sized,
    {
        let request_len = request.encoded_len()?;
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        let prepared = registry.prepare_json_call(self, address)?;
        let capacity = prepared.request_frame_capacity();
        if request_len > capacity {
            return Err(RpcError::FrameTooLarge {
                size: request_len,
                capacity,
            });
        }
        let (writer, reader) = super::payload::make_payload_call(prepared);
        Ok(JsonCall::new(request, request_len, writer, reader))
    }

    /// Starts a format-agnostic payload call to the endpoint at `address`.
    ///
    /// The returned [`RpcPayloadWriter`] and [`RpcPayloadReader`] expose
    /// full-duplex asynchronous frame IO. The registered native or JSON adapter
    /// determines how each frame is interpreted. Response and method-error
    /// frames remain borrowed from the lane until dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry was dropped, the endpoint does not
    /// exist, identifiers are exhausted, or this is a direct synchronous
    /// self-call. The returned stream may later report lane, frame, or typed
    /// request validation failures.
    pub fn call_payload(
        &self,
        address: &RpcAddress,
    ) -> RpcResult<(RpcPayloadWriter, RpcPayloadReader)> {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        let prepared = registry.prepare_payload_call(self, address)?;
        Ok(super::payload::make_payload_call(prepared))
    }

    /// Returns a read-only projection of the method registered at `address`:
    /// signature identity checks and cardinality.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::RegistryDropped`] when the registry is gone, or
    /// [`RpcError::NotFound`] when no endpoint owns `address`.
    pub fn method_info(&self, address: &RpcAddress) -> RpcResult<RpcMethodInfo> {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        registry.method_info(address)
    }

    /// Returns the schema and byte limits registered for a JSON method.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry is gone, the address is absent, or
    /// the endpoint is not a JSON method.
    pub fn json_method_info(&self, address: &RpcAddress) -> RpcResult<JsonRpcInfo> {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        registry.json_method_info(address)
    }

    /// Starts a wire-level multicast with one shared request direction.
    ///
    /// Every target must register the same request message type and request
    /// cardinality. Each returned branch preserves its target's independent
    /// response and method-error stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the target list is empty, an address is missing,
    /// target inputs differ, or call preparation otherwise fails.
    pub fn multicast_payload(
        &self,
        addresses: &[RpcAddress],
    ) -> RpcResult<(RpcPayloadWriter, Vec<RpcMulticastBranch>)> {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        let prepared = registry.prepare_multicast_payload(self, addresses)?;
        Ok(super::payload::make_payload_calls(
            prepared,
            addresses.to_vec(),
        ))
    }

    /// Starts a typed-request multicast and returns independent wire response branches.
    ///
    /// `T` and `Mode` describe the one request stream shared by every target.
    /// Responses remain wire-level because multicast targets may declare
    /// different response, error, and output contracts. Polling any returned
    /// branch also drives request encoding and every target handler.
    ///
    /// # Errors
    ///
    /// Returns an error when a target is absent, its input contract differs
    /// from `T + Mode`, the target list is empty, or call preparation fails.
    pub fn multicast<T, Mode>(
        &self,
        addresses: &[RpcAddress],
        input: <Mode as RpcInputMode<T>>::ClientInput,
    ) -> RpcResult<Vec<RpcMulticastBranch>>
    where
        T: RpcMessage,
        Mode: RpcInputMode<T>,
    {
        let registry = self.registry.upgrade().ok_or(RpcError::RegistryDropped)?;
        let prepared = registry.prepare_typed_multicast::<T, Mode>(self, addresses)?;
        let (writer, branches) = super::payload::make_payload_calls(prepared, addresses.to_vec());
        Ok(super::typed::make_typed_multicast::<T, Mode>(
            input, writer, branches,
        ))
    }
}

/// Identity token for safely unregistering one endpoint instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRegistration {
    /// Registered address.
    address: RpcAddress,
    /// Registered endpoint instance identity.
    endpoint_id: RpcEndpointId,
}

/// Transport/runtime error returned while registering or invoking RPC endpoints.
///
/// Method-specific business errors are carried as typed method-error frames
/// and are not represented by this enum. Wire-level callers receive the same
/// frame through [`RpcPayloadReader`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcError {
    /// Address parsing failed at an API boundary.
    #[error(transparent)]
    Address(#[from] RpcAddressError),
    /// A second handler attempted to occupy an existing address.
    #[error("RPC endpoint is already registered: {0}")]
    AlreadyRegistered(RpcAddress),
    /// No handler currently owns the requested address.
    #[error("RPC endpoint not found: {0}")]
    NotFound(RpcAddress),
    /// A typed API was used with a lane-native JSON endpoint.
    #[error("RPC endpoint is not typed: {0}")]
    NotTypedEndpoint(RpcAddress),
    /// A JSON API was used with a typed endpoint.
    #[error("RPC endpoint is not JSON: {0}")]
    NotJsonEndpoint(RpcAddress),
    /// A JSON document was syntactically invalid or not UTF-8.
    #[error("invalid JSON document")]
    InvalidJson,
    /// An endpoint attempted to synchronously invoke itself.
    #[error("direct RPC self-call is forbidden: {0}")]
    DirectSelfCall(RpcAddress),
    /// The registry behind an [`RpcClient`] no longer exists.
    #[error("RPC registry was dropped")]
    RegistryDropped,
    /// The endpoint or call identifier space was exhausted.
    #[error("RPC identifier space exhausted")]
    IdentifierExhausted,
    /// An unregister token no longer identifies the current endpoint instance.
    #[error("stale RPC registration: {0}")]
    StaleRegistration(RpcAddress),
    /// A Component unload revoked the endpoint while this call was in flight.
    #[error("RPC endpoint was revoked: {0}")]
    EndpointRevoked(RpcAddress),
    /// The client method marker does not match the registered typed endpoint.
    #[error("RPC signature mismatch at {address}: expected {expected}, registered {registered:?}")]
    SignatureMismatch {
        /// Invoked address.
        address: RpcAddress,
        /// Signature requested by the client.
        expected: &'static str,
        /// Signature registered at the address.
        registered: &'static str,
    },
    /// A multicast call contained no target addresses.
    #[error("RPC multicast requires at least one target")]
    EmptyMulticast,
    /// Two multicast targets do not accept the same request contract.
    #[error("RPC multicast input mismatch between {expected} and {actual}")]
    MulticastInputMismatch {
        /// Address defining the expected request contract.
        expected: RpcAddress,
        /// Address whose request contract differs.
        actual: RpcAddress,
    },
    /// The typed multicast caller selected an input contract a target does not accept.
    #[error(
        "RPC multicast caller input mismatch at {address}: expected {expected_request} + {expected_mode}, registered {registered_request} + {registered_mode}"
    )]
    MulticastCallerInputMismatch {
        /// Target whose input differs from the caller's input.
        address: RpcAddress,
        /// Request type selected by the caller.
        expected_request: &'static str,
        /// Input cardinality selected by the caller.
        expected_mode: &'static str,
        /// Request type registered by the target.
        registered_request: &'static str,
        /// Input cardinality registered by the target.
        registered_mode: &'static str,
    },
    /// One call requested more lanes than fixed storage can provide.
    #[error("RPC call requires {requested} lanes but storage contains {limit}")]
    LaneBatchTooLarge {
        /// Lanes required atomically by the call.
        requested: usize,
        /// Configured active lane count.
        limit: usize,
    },
    /// Internal framing state became inconsistent.
    #[error("invalid RPC frame decoder state")]
    InvalidFrameState,
    /// A unary side reached EOF without carrying a message.
    #[error("unary RPC side did not contain a message")]
    MissingUnaryFrame,
    /// A completed unary call future was polled again.
    #[error("completed unary RPC call was polled again")]
    CompletedCallPolled,
    /// Fixed lane storage has no free slot for a nested call.
    #[error("nested RPC call cannot acquire one of {limit} lanes without deadlocking")]
    NestedLaneExhausted {
        /// Configured active lane count.
        limit: usize,
    },
    /// A handler-owned fixed-capacity resource has no free slot.
    #[error("{resource} capacity {limit} is exhausted")]
    ResourceExhausted {
        /// Stable name of the exhausted resource.
        resource: &'static str,
        /// Configured resource capacity.
        limit: usize,
    },
    /// The bounded root-call waiter table is full.
    #[error("RPC lane waiter capacity {limit} is exhausted")]
    LaneWaiterCapacityExceeded {
        /// Configured waiter capacity.
        limit: usize,
    },
    /// Fixed lane state became internally inconsistent.
    #[error("invalid RPC lane state")]
    InvalidLaneState,
    /// An RPC frame writer was already closed.
    #[error("RPC frame writer is closed")]
    FrameWriterClosed,
    /// An RPC frame receiver was dropped before the writer completed.
    #[error("RPC frame reader is closed")]
    FrameReaderClosed,
    /// Frame bytes do not satisfy a message's size, alignment, or validity.
    #[error("invalid fixed-layout RPC frame for {message_type}")]
    InvalidMessageFrame {
        /// Rust message type that rejected the bytes.
        message_type: &'static str,
    },
    /// A runtime-sized payload exceeds the available bytes for its write.
    #[error("RPC frame size {size} exceeds available capacity {capacity}")]
    FrameTooLarge {
        /// Requested payload size.
        size: usize,
        /// Available bytes for this write.
        capacity: usize,
    },
    /// A non-empty `write_all` operation could not make forward progress.
    #[error("RPC payload writer accepted zero bytes")]
    PayloadWriteZero,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::indexing_slicing)]
    #![allow(missing_docs)]

    use alloc::rc::Rc;
    use core::cell::Cell;

    use futures_lite::future::{block_on, poll_once};
    use futures_util::stream;

    use super::*;
    use crate::{RpcFrame, RpcLaneStorage, RpcMethod, RpcStream, Streaming, Unary};

    struct MulticastUnaryA;

    impl RpcMethod for MulticastUnaryA {
        const ADDRESS: &'static str = "multicast.unary_a";
        type Request = [u8; 8];
        type Response = [u8; 8];
        type Error = [u8; 2];
        type Input = Unary;
        type Output = Unary;
    }

    struct MulticastUnaryB;

    impl RpcMethod for MulticastUnaryB {
        const ADDRESS: &'static str = "multicast.unary_b";
        type Request = [u8; 8];
        type Response = [u8; 4];
        type Error = [u8; 3];
        type Input = Unary;
        type Output = Unary;
    }

    struct DifferentInput;

    impl RpcMethod for DifferentInput {
        const ADDRESS: &'static str = "multicast.different_input";
        type Request = [u8; 4];
        type Response = [u8; 4];
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    struct MulticastStreamA;

    impl RpcMethod for MulticastStreamA {
        const ADDRESS: &'static str = "multicast.stream_a";
        type Request = [u8; 8];
        type Response = [u8; 8];
        type Error = [u8; 1];
        type Input = Streaming;
        type Output = Streaming;
    }

    struct MulticastStreamB;

    impl RpcMethod for MulticastStreamB {
        const ADDRESS: &'static str = "multicast.stream_b";
        type Request = [u8; 8];
        type Response = [u8; 8];
        type Error = [u8; 2];
        type Input = Streaming;
        type Output = Streaming;
    }

    fn address<M: RpcMethod>() -> RpcAddress {
        RpcAddress::try_from(M::ADDRESS).expect("valid test RPC address")
    }

    #[test]
    fn multicast_request_is_shared_and_responses_remain_independent() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
            let registry = RpcRegistry::new(lanes);
            let pointer_a = Rc::new(Cell::new(0_usize));
            let pointer_b = Rc::new(Cell::new(0_usize));

            let handler_pointer = Rc::clone(&pointer_a);
            registry
                .register::<MulticastUnaryA, _>(
                    "system",
                    move |_context, request: RpcFrame<[u8; 8]>| {
                        let pointer = Rc::clone(&handler_pointer);
                        async move {
                            pointer.set(request.view()?.as_ptr() as usize);
                            Ok(Ok(*b"response"))
                        }
                    },
                )
                .expect("register first multicast endpoint");

            let handler_pointer = Rc::clone(&pointer_b);
            registry
                .register::<MulticastUnaryB, _>(
                    "system",
                    move |_context, request: RpcFrame<[u8; 8]>| {
                        let pointer = Rc::clone(&handler_pointer);
                        async move {
                            pointer.set(request.view()?.as_ptr() as usize);
                            Ok(Ok(*b"done"))
                        }
                    },
                )
                .expect("register second multicast endpoint");

            let addresses = [address::<MulticastUnaryA>(), address::<MulticastUnaryB>()];
            let (mut writer, branches) = registry
                .client()
                .multicast_payload(&addresses)
                .expect("prepare multicast call");
            let mut branches = branches.into_iter();
            let mut branch_a = branches.next().expect("first multicast branch");
            let mut branch_b = branches.next().expect("second multicast branch");
            assert!(branches.next().is_none());

            writer
                .write_all(b"request-")
                .await
                .expect("publish shared request");
            writer.close().await.expect("close multicast request");

            let response_a = branch_a
                .reader_mut()
                .read()
                .await
                .expect("read first response")
                .expect("first response frame")
                .expect("first method success");
            assert_eq!(response_a.as_ref(), b"response");
            drop(response_a);

            let response_b = branch_b
                .reader_mut()
                .read()
                .await
                .expect("read second response")
                .expect("second response frame")
                .expect("second method success");
            assert_eq!(response_b.as_ref(), b"done");
            assert_ne!(pointer_a.get(), 0);
            assert_eq!(pointer_a.get(), pointer_b.get());
        });
    }

    #[test]
    fn typed_multicast_encodes_one_input_and_drives_each_branch() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
            let registry = RpcRegistry::new(lanes);
            registry
                .register::<MulticastUnaryA, _>(
                    "system",
                    |_context, request: RpcFrame<[u8; 8]>| async move {
                        assert_eq!(request.view()?, b"request-");
                        Ok(Ok(*b"response"))
                    },
                )
                .expect("register first typed multicast endpoint");
            registry
                .register::<MulticastUnaryB, _>(
                    "system",
                    |_context, request: RpcFrame<[u8; 8]>| async move {
                        assert_eq!(request.view()?, b"request-");
                        Ok(Ok(*b"done"))
                    },
                )
                .expect("register second typed multicast endpoint");

            let addresses = [address::<MulticastUnaryA>(), address::<MulticastUnaryB>()];
            let branches = registry
                .client()
                .multicast::<[u8; 8], Unary>(&addresses, *b"request-")
                .expect("prepare typed multicast");
            let mut branches = branches.into_iter();
            let mut branch_a = branches.next().expect("first typed branch");
            let mut branch_b = branches.next().expect("second typed branch");

            let response_a = branch_a
                .reader_mut()
                .read()
                .await
                .expect("read first typed branch")
                .expect("first typed response")
                .expect("first typed success");
            assert_eq!(response_a.as_ref(), b"response");
            drop(response_a);

            let response_b = branch_b
                .reader_mut()
                .read()
                .await
                .expect("read second typed branch")
                .expect("second typed response")
                .expect("second typed success");
            assert_eq!(response_b.as_ref(), b"done");
        });
    }

    #[test]
    fn multicast_rejects_mismatched_request_types_before_io() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<MulticastUnaryA, _>(
                "system",
                |_context, _request: RpcFrame<[u8; 8]>| async { Ok(Ok(*b"response")) },
            )
            .expect("register matching endpoint");
        registry
            .register::<DifferentInput, _>(
                "system",
                |_context, request: RpcFrame<[u8; 4]>| async move { Ok(Ok(*request.view()?)) },
            )
            .expect("register mismatched endpoint");

        let addresses = [address::<MulticastUnaryA>(), address::<DifferentInput>()];
        assert!(matches!(
            registry.client().multicast_payload(&addresses),
            Err(RpcError::MulticastInputMismatch { .. })
        ));
    }

    #[test]
    fn multicast_writer_waits_for_every_request_reader() {
        block_on(async {
            let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
            let registry = RpcRegistry::new(lanes);
            let allow_second_reader = Rc::new(Cell::new(false));

            registry
                .register::<MulticastStreamA, _>(
                    "system",
                    |_context, requests: RpcStream<RpcFrame<[u8; 8]>>| async move {
                        Ok(RpcStream::new(stream::unfold(
                            requests,
                            |mut requests| async move {
                                requests.next().await.map(|request| {
                                    let response =
                                        request.and_then(|frame| frame.view().copied()).map(Ok);
                                    (response, requests)
                                })
                            },
                        )))
                    },
                )
                .expect("register first streaming endpoint");
            let handler_gate = Rc::clone(&allow_second_reader);
            registry
                .register::<MulticastStreamB, _>(
                    "system",
                    move |_context, requests: RpcStream<RpcFrame<[u8; 8]>>| {
                        let gate = Rc::clone(&handler_gate);
                        async move {
                            core::future::poll_fn(|_context| {
                                if gate.get() {
                                    core::task::Poll::Ready(())
                                } else {
                                    core::task::Poll::Pending
                                }
                            })
                            .await;
                            Ok(RpcStream::new(stream::unfold(
                                requests,
                                |mut requests| async move {
                                    requests.next().await.map(|request| {
                                        let response =
                                            request.and_then(|frame| frame.view().copied()).map(Ok);
                                        (response, requests)
                                    })
                                },
                            )))
                        }
                    },
                )
                .expect("register second streaming endpoint");

            let addresses = [address::<MulticastStreamA>(), address::<MulticastStreamB>()];
            let (mut writer, branches) = registry
                .client()
                .multicast_payload(&addresses)
                .expect("prepare streaming multicast");
            let mut branches = branches.into_iter();
            let mut branch_a = branches.next().expect("first branch");
            let mut branch_b = branches.next().expect("second branch");

            writer
                .write_all(b"frame-01")
                .await
                .expect("publish first frame");

            let response_a = branch_a
                .reader_mut()
                .read()
                .await
                .expect("read first branch")
                .expect("first branch frame")
                .expect("first branch success");
            drop(response_a);

            assert!(poll_once(writer.reserve()).await.is_none());

            allow_second_reader.set(true);

            let response_b = branch_b
                .reader_mut()
                .read()
                .await
                .expect("read second branch")
                .expect("second branch frame")
                .expect("second branch success");
            drop(response_b);

            let reservation = writer.reserve().await.expect("reserve after every reader");
            drop(reservation);
        });
    }
}
