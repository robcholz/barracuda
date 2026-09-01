//! Fixed Event Router RPC projection for Agent tool discovery.

use alloc::{boxed::Box, format, string::String, string::ToString, vec::Vec};

use barracuda_agent_runtime::tools::{
    Action, RiskClass, Tool, ToolArgumentsValidator, ToolError, ToolFuture, ToolGroup,
    ToolGroupProvider, ToolGroupProviderError, ToolHandler, ToolInvocation, ToolInvokeError,
    ToolOutput, ToolResult, ToolSpec,
};
use barracuda_event_router::{RpcAddress, RpcCardinality, RpcClient, RpcError, RpcMethodInfo};
use serde_json::Value;

/// One-time Agent tool projection of the registered Event Router RPC graph.
pub(crate) struct EventRouterToolProvider {
    rpc: RpcClient,
}

impl EventRouterToolProvider {
    pub(crate) const fn new(rpc: RpcClient) -> Self {
        Self { rpc }
    }
}

impl ToolGroupProvider for EventRouterToolProvider {
    fn provide(&self) -> Result<Vec<ToolGroup>, ToolGroupProviderError> {
        let mut projected = Vec::new();
        for group in self.rpc.groups().map_err(source_error)? {
            let mut tools = Vec::new();
            for address in self.rpc.rpcs(&group).map_err(source_error)? {
                let info = self.rpc.method_info(&address).map_err(source_error)?;
                if info.input_mode() != RpcCardinality::Unary
                    || info.output_mode() != RpcCardinality::Unary
                {
                    continue;
                }
                let Some(request_schema) = info.schema() else {
                    continue;
                };
                tools.push(project_tool(
                    self.rpc.clone(),
                    address,
                    info,
                    request_schema,
                )?);
            }
            if !tools.is_empty() {
                projected.push(ToolGroup::new(group.as_ref(), false, tools));
            }
        }
        Ok(projected)
    }
}

fn project_tool(
    rpc: RpcClient,
    address: RpcAddress,
    info: RpcMethodInfo,
    request_schema: &str,
) -> Result<Tool, ToolGroupProviderError> {
    let validator = json_validator::OwnedValidator::from_json(request_schema).map_err(|error| {
        ToolGroupProviderError::new(format!(
            "unsupported baked schema for RPC `{address}`: {error}"
        ))
    })?;
    let parameters: Value = serde_json::from_str(request_schema).map_err(|error| {
        ToolGroupProviderError::new(format!("invalid baked schema for RPC `{address}`: {error}"))
    })?;
    if !parameters.is_object() {
        return Err(ToolGroupProviderError::new(format!(
            "baked schema for RPC `{address}` must be a JSON object"
        )));
    }
    let name = tool_name(&address);
    let schema = serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": format!("Call Event Router RPC `{address}`."),
            "parameters": parameters
        }
    })
    .to_string();
    Ok(Tool::new(EventRouterRpcTool {
        validator: RpcArgumentsValidator {
            address: address.clone(),
            info,
            schema: validator,
        },
        name,
        schema,
        address,
        rpc,
    }))
}

struct EventRouterRpcTool {
    validator: RpcArgumentsValidator,
    name: String,
    schema: String,
    address: RpcAddress,
    rpc: RpcClient,
}

impl ToolSpec for EventRouterRpcTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &str {
        &self.schema
    }

    fn arguments_validator(&self) -> &dyn ToolArgumentsValidator {
        &self.validator
    }

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::High)
    }
}

impl ToolHandler for EventRouterRpcTool {
    type Args = Value;

    fn invoke<'a>(&'a self, arguments: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let response = self
                .rpc
                .call_json(&self.address, &arguments)
                .await
                .map_err(invoke_rejected)?;
            let ok = response.get("ok").and_then(Value::as_bool) == Some(true);
            Ok(ToolOutput {
                content: response.to_string(),
                ok,
            })
        })
    }
}

struct RpcArgumentsValidator {
    address: RpcAddress,
    info: RpcMethodInfo,
    schema: json_validator::OwnedValidator,
}

impl ToolArgumentsValidator for RpcArgumentsValidator {
    fn validate(&self, arguments: &Value) -> ToolResult<()> {
        self.schema
            .validate(arguments)
            .map_err(ToolError::from)
            .map_err(ToolInvokeError::from)?;
        self.info
            .encode_request(&self.address, arguments)
            .map(|_| ())
            .map_err(|error| ToolInvokeError::new(ToolError::InvalidArguments(error.to_string())))
    }
}

fn tool_name(address: &RpcAddress) -> String {
    match address.as_ref().split_once('.') {
        Some((group, method)) => format!("rpc_{}_{}_{}", group.len(), group, method),
        None => format!("rpc_{}", address.as_ref()),
    }
}

fn source_error(error: RpcError) -> ToolGroupProviderError {
    ToolGroupProviderError::new(error.to_string())
}

fn invoke_rejected(error: RpcError) -> ToolInvokeError {
    ToolError::InvokeRejected(error.to_string()).into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use alloc::boxed::Box;
    use barracuda_event_router::{
        Dynamic, JsonCodec, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, RpcStream, RpcWire,
        Streaming, Unary, WireSupport,
    };
    use serde::{Deserialize, Serialize};
    use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

    const REQUEST_SCHEMA: &str = r#"{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}"#;

    #[repr(C)]
    #[derive(
        Clone,
        Copy,
        Deserialize,
        Immutable,
        IntoBytes,
        KnownLayout,
        RpcWire,
        Serialize,
        TryFromBytes,
    )]
    struct Request {
        value: u32,
    }

    struct Visible;

    impl RpcMethod for Visible {
        const ADDRESS: &'static str = "visible.call";
        type Request = Request;
        type Response = Request;
        type Error = ();
        type Input = Unary;
        type Output = Unary;

        fn dynamic() -> Option<Dynamic> {
            Some(Dynamic::new(
                JsonCodec::of::<Self>(),
                WireSupport::of::<Self>(),
                Some(REQUEST_SCHEMA),
            ))
        }
    }

    struct StreamingOutput;

    impl RpcMethod for StreamingOutput {
        const ADDRESS: &'static str = "visible.watch";
        type Request = Request;
        type Response = Request;
        type Error = ();
        type Input = Unary;
        type Output = Streaming;

        fn dynamic() -> Option<Dynamic> {
            Some(Dynamic::new(
                JsonCodec::of::<Self>(),
                WireSupport::of::<Self>(),
                Some(REQUEST_SCHEMA),
            ))
        }
    }

    #[test]
    fn provider_projects_the_registered_graph_once() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 64, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        let provider = EventRouterToolProvider::new(registry.client());

        let registration = registry
            .register::<Visible, _>(|_context, request: RpcFrame<Request>| async move {
                Ok(Ok(*request.view()?))
            })
            .expect("register visible RPC");
        let groups = provider.provide().expect("project registered RPC");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups.first().map(ToolGroup::id), Some("visible"));
        registry
            .unregister(&registration)
            .expect("clean up endpoint");
    }

    #[test]
    fn provider_projects_only_unary_unary_methods() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<StreamingOutput, _>(|_context, _request: RpcFrame<Request>| async move {
                Ok(RpcStream::new(futures_lite::stream::empty()))
            })
            .expect("register streaming RPC");

        let provider = EventRouterToolProvider::new(registry.client());
        assert!(provider.provide().expect("project RPCs").is_empty());
    }

    #[test]
    fn rpc_identity_encoding_is_injective_for_ambiguous_segments() {
        let left = RpcAddress::try_from("a.b_c").expect("valid address");
        let right = RpcAddress::try_from("a_b.c").expect("valid address");

        assert_eq!(tool_name(&left), "rpc_1_a_b_c");
        assert_eq!(tool_name(&right), "rpc_3_a_b_c");
        assert_ne!(tool_name(&left), tool_name(&right));
    }

    #[test]
    fn validator_uses_the_registered_dynamic_codec() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry
            .register::<Visible, _>(|_context, request: RpcFrame<Request>| async move {
                Ok(Ok(*request.view()?))
            })
            .expect("register visible RPC");
        let client = registry.client();
        let address = RpcAddress::try_from(Visible::ADDRESS).expect("valid address");
        let validator = RpcArgumentsValidator {
            info: client.method_info(&address).expect("method info"),
            address,
            schema: json_validator::OwnedValidator::from_json(REQUEST_SCHEMA)
                .expect("compile schema"),
        };

        assert!(validator.validate(&serde_json::json!({"value": 7})).is_ok());
        assert!(validator.validate(&serde_json::json!({})).is_err());
        assert!(validator
            .validate(&serde_json::json!({"value": 7, "extra": true}))
            .is_err());
    }
}
