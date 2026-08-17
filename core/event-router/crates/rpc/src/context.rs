use super::registry::RpcClient;
use getset::{CopyGetters, Getters};

/// Identity of one RPC invocation.
#[derive(Clone, Copy, CopyGetters, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RpcCallId {
    /// Numeric call identifier.
    #[getset(get_copy = "pub")]
    value: u64,
}

impl RpcCallId {
    pub(crate) fn new(value: u64) -> Self {
        Self { value }
    }
}

/// Identity of one registered endpoint instance.
#[derive(Clone, Copy, CopyGetters, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RpcEndpointId {
    /// Numeric endpoint identifier.
    #[getset(get_copy = "pub")]
    value: u64,
}

impl RpcEndpointId {
    pub(crate) fn new(value: u64) -> Self {
        Self { value }
    }
}

/// Metadata and nested-call client supplied to an RPC handler.
#[derive(Clone, CopyGetters, Getters)]
pub struct RpcContext {
    /// This invocation's identity.
    #[getset(get_copy = "pub")]
    call_id: RpcCallId,
    /// Root invocation identity shared by the nested call chain.
    #[getset(get_copy = "pub")]
    root_call_id: RpcCallId,
    /// Immediate parent invocation, if this is a nested call.
    #[getset(get_copy = "pub")]
    parent_call_id: Option<RpcCallId>,
    /// Endpoint that initiated this call, if any.
    #[getset(get_copy = "pub")]
    caller_endpoint_id: Option<RpcEndpointId>,
    /// Endpoint handling this invocation.
    #[getset(get_copy = "pub")]
    endpoint_id: RpcEndpointId,
    /// Client bound to the current endpoint and call lineage.
    #[getset(get = "pub")]
    client: RpcClient,
}

impl RpcContext {
    pub(crate) fn new(
        call_id: RpcCallId,
        root_call_id: RpcCallId,
        parent_call_id: Option<RpcCallId>,
        caller_endpoint_id: Option<RpcEndpointId>,
        endpoint_id: RpcEndpointId,
        client: RpcClient,
    ) -> Self {
        Self {
            call_id,
            root_call_id,
            parent_call_id,
            caller_endpoint_id,
            endpoint_id,
            client,
        }
    }
}
