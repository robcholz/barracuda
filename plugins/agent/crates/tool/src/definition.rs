use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_rpc::{JsonRpcInfo, RpcClient};
use getset::CopyGetters;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::validate;

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult<ToolOutput>> + 'a>>;
pub type ToolCompletionFuture = Pin<Box<dyn Future<Output = ToolResult<ToolOutput>> + 'static>>;
pub type DetachedToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult<DetachedTool>> + 'a>>;
pub type ToolResult<T> = Result<T, ToolInvokeError>;

/// The two settlements produced by a dynamically detached tool.
///
/// The accepted output is returned to the current model turn. The completion
/// future is transferred to the owning Agent runtime and may finish after that
/// turn has ended.
pub struct DetachedTool {
    accepted: ToolOutput,
    completion: ToolCompletionFuture,
}

impl DetachedTool {
    pub fn new(accepted: ToolOutput, completion: ToolCompletionFuture) -> Self {
        Self {
            accepted,
            completion,
        }
    }

    pub(crate) fn into_parts(self) -> (ToolOutput, ToolCompletionFuture) {
        (self.accepted, self.completion)
    }
}

/// Framework-only execution configuration. It is never rendered into a tool's
/// model-facing schema or usage text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolConfig {
    /// Return an acceptance receipt and move execution into the owning runtime.
    pub detached: bool,
}

/// Serde projection for a schema that accepts an empty argument object.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct EmptyArgs {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInvocation {
    id: Option<String>,
    name: String,
    arguments_json: String,
}

impl ToolInvocation {
    pub fn try_new(id: Option<&str>, name: &str, arguments_json: &str) -> ToolResult<Self> {
        let arguments_json = validate::normalize_arguments_json(arguments_json)?;
        Ok(Self {
            id: id.map(str::to_owned),
            name: name.to_owned(),
            arguments_json: arguments_json.to_owned(),
        })
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments_json(&self) -> &str {
        &self.arguments_json
    }

    /// Deserialize the raw argument object into a tool-specific DTO.
    pub fn arguments<'de, T>(&'de self) -> ToolResult<T>
    where
        T: Deserialize<'de>,
    {
        serde_json::from_str(&self.arguments_json)
            .map_err(|error| ToolInvokeError::new(ToolError::InvalidArguments(error.to_string())))
    }
}

#[cfg(test)]
mod invocation_tests {
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::string::String;
    use core::cell::Cell;

    use super::{Tool, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec};
    use barracuda_rpc::{
        JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcAddress, RpcLaneStorage, RpcRegistry,
    };
    use futures_lite::future::block_on;
    use serde::Deserialize;

    struct BusinessHandler {
        invoked: Rc<Cell<bool>>,
    }

    impl ToolSpec for BusinessHandler {
        fn name(&self) -> &str {
            "example"
        }

        fn schema(&self) -> &str {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/example.json"
            ))
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("tests/fixtures/example.json");
            &VALIDATOR
        }
    }

    impl ToolHandler for BusinessHandler {
        type Args = BusinessArgs;

        fn invoke<'a>(&'a self, arguments: Self::Args) -> ToolFuture<'a> {
            self.invoked.set(true);
            Box::pin(async move {
                Ok(ToolOutput {
                    content: arguments.name,
                    ok: arguments.enabled,
                })
            })
        }
    }

    #[derive(Deserialize)]
    struct BusinessArgs {
        name: String,
        enabled: bool,
    }

    #[test]
    fn invocation_deserializes_from_its_raw_json() -> Result<(), Box<dyn core::error::Error>> {
        let invocation =
            ToolInvocation::try_new(None, "example", r#"{"name":"lamp","enabled":true}"#)?;
        let arguments: BusinessArgs = invocation.arguments()?;
        assert_eq!(arguments.name, "lamp");
        assert!(arguments.enabled);
        Ok(())
    }

    struct RpcEcho;

    impl JsonRpcSchema for RpcEcho {
        const ADDRESS: &'static str = "demo.rpc_echo";
        const REQUEST_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
            r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}"#
        );
        const RESPONSE_SCHEMA: JsonSchema = barracuda_rpc::json_schema_inline!(
            r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}"#
        );
        const MAX_REQUEST_BYTES: usize = 128;
        const MAX_RESPONSE_BYTES: usize = 128;
    }

    #[test]
    fn json_rpc_tool_uses_the_rpc_address_schema_and_lane(
    ) -> Result<(), Box<dyn core::error::Error>> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry.register_json::<RpcEcho, _>(
            "agent",
            |_context, request: JsonRef, response: JsonWriter| async move {
                response.write(request.as_str()?).await
            },
        )?;
        let client = registry.client();
        let address = RpcAddress::try_from(RpcEcho::ADDRESS)?;
        let info = client.json_method_info(&address)?;
        let tool = Tool::from_json_rpc(client, info);

        assert_eq!(tool.name(), RpcEcho::ADDRESS);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(tool.schema())?,
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": RpcEcho::ADDRESS,
                    "parameters": serde_json::from_str::<serde_json::Value>(
                        RpcEcho::REQUEST_SCHEMA.as_str()
                    )?
                }
            })
        );

        let call = ToolInvocation::try_new(None, RpcEcho::ADDRESS, r#"{"message":"hello"}"#)?;
        let output = block_on(tool.invoke(&call))?;
        assert_eq!(output.content, r#"{"message":"hello"}"#);
        assert!(output.ok);
        Ok(())
    }

    #[test]
    fn json_rpc_tool_reuses_the_rpc_request_validator_before_calling(
    ) -> Result<(), Box<dyn core::error::Error>> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 128, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        let invoked = Rc::new(Cell::new(false));
        let handler_invoked = Rc::clone(&invoked);
        registry.register_json::<RpcEcho, _>(
            "agent",
            move |_context, _request: JsonRef, response: JsonWriter| {
                handler_invoked.set(true);
                async move { response.write(r#"{"message":"unexpected"}"#).await }
            },
        )?;
        let client = registry.client();
        let address = RpcAddress::try_from(RpcEcho::ADDRESS)?;
        let tool = Tool::from_json_rpc(client.clone(), client.json_method_info(&address)?);
        let call = ToolInvocation::try_new(None, RpcEcho::ADDRESS, r#"{"message":1}"#)?;

        assert!(matches!(
            tool.classify(&call),
            Err(error) if matches!(error.error, ToolError::ArgumentsSchema(_))
        ));
        assert!(matches!(
            block_on(tool.invoke(&call)),
            Err(error) if matches!(error.error, ToolError::ArgumentsSchema(_))
        ));
        assert!(!invoked.get());
        Ok(())
    }

    #[test]
    fn framework_validates_schema_before_entering_business_handler(
    ) -> Result<(), Box<dyn core::error::Error>> {
        let invoked = Rc::new(Cell::new(false));
        let tool = Tool::new(BusinessHandler {
            invoked: Rc::clone(&invoked),
        });
        let invalid = ToolInvocation::try_new(None, "example", r#"{"name":"lamp"}"#)?;

        let classification_error = tool.classify(&invalid).err().ok_or("expected error")?;
        assert!(matches!(
            classification_error.error,
            ToolError::ArgumentsSchema(_)
        ));

        let error = block_on(tool.invoke(&invalid))
            .err()
            .ok_or("expected error")?;

        assert!(matches!(error.error, ToolError::ArgumentsSchema(_)));
        assert!(!invoked.get());

        let blank =
            ToolInvocation::try_new(None, "example", r#"{"name":"  \t\n","enabled":true}"#)?;
        let error = block_on(tool.invoke(&blank))
            .err()
            .ok_or("expected error")?;
        assert!(matches!(error.error, ToolError::ArgumentsSchema(_)));
        assert!(!invoked.get());
        Ok(())
    }

    #[test]
    fn business_handler_receives_validated_arguments() -> Result<(), Box<dyn core::error::Error>> {
        let invoked = Rc::new(Cell::new(false));
        let tool = Tool::new(BusinessHandler {
            invoked: Rc::clone(&invoked),
        });
        let valid = ToolInvocation::try_new(None, "example", r#"{"name":"lamp","enabled":true}"#)?;

        let output = block_on(tool.invoke(&valid))?;

        assert!(invoked.get());
        assert_eq!(output.content, "lamp");
        assert!(output.ok);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments json: {0}")]
    InvalidArgumentsJson(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("invalid arguments: {0}")]
    ArgumentsSchema(#[from] json_validator::ValidationError),
    #[error("RPC call failed: {0}")]
    Rpc(#[from] barracuda_rpc::RpcError),
    #[error("tool invocation rejected: {0}")]
    InvokeRejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInvokeError {
    pub error: ToolError,
}

impl ToolInvokeError {
    pub fn new(error: ToolError) -> Self {
        Self { error }
    }
}

impl fmt::Display for ToolInvokeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for ToolInvokeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<ToolError> for ToolInvokeError {
    fn from(error: ToolError) -> Self {
        Self::new(error)
    }
}

pub trait ToolSpec {
    fn name(&self) -> &str;

    fn schema(&self) -> &str;

    fn arguments_validator(&self) -> &'static json_validator::Validator;

    fn usage(&self) -> Option<&str> {
        None
    }

    fn concurrent(&self) -> bool {
        false
    }

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::High)
    }
}

pub trait ToolHandler: ToolSpec {
    type Args: DeserializeOwned;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a>;
}

/// A tool whose accepted and completed settlements become available at
/// different times.
pub trait DetachedToolHandler: ToolSpec {
    type Args: DeserializeOwned;

    fn invoke<'a>(&'a self, args: Self::Args) -> DetachedToolFuture<'a>;
}

trait ErasedToolHandler: ToolSpec {
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a>;
}

impl<Handler> ErasedToolHandler for Handler
where
    Handler: ToolHandler,
{
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> ToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments::<Handler::Args>()?;
            self.invoke(args).await
        })
    }
}

trait ErasedDetachedToolHandler: ToolSpec {
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> DetachedToolFuture<'a>;
}

impl<Handler> ErasedDetachedToolHandler for Handler
where
    Handler: DetachedToolHandler,
{
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> DetachedToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments::<Handler::Args>()?;
            self.invoke(args).await
        })
    }
}

#[macro_export]
macro_rules! tool_metadata {
    ($name:literal) => {
        fn name(&self) -> &str {
            $name
        }

        fn schema(&self) -> &str {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/resources/tools/",
                $name,
                "/schema.json"
            ))
        }

        fn arguments_validator(&self) -> &'static ::json_validator::Validator {
            const VALIDATOR: ::json_validator::Validator =
                ::json_validator::validator!("resources/tools/", $name, "/schema.json");
            &VALIDATOR
        }

        fn usage(&self) -> ::core::option::Option<&str> {
            const USAGE: &str = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/resources/tools/",
                $name,
                "/usage.md"
            ));
            if USAGE.trim().is_empty() {
                ::core::option::Option::None
            } else {
                ::core::option::Option::Some(USAGE)
            }
        }
    };
}

/// Embed several canonical `resources/tools/<name>/schema.json` documents as
/// one provider-ready JSON array.
#[macro_export]
macro_rules! tool_schemas {
    ($first:literal $(, $rest:literal)* $(,)?) => {
        concat!(
            "[",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/resources/tools/",
                $first,
                "/schema.json"
            )),
            $(
                ",",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/resources/tools/",
                    $rest,
                    "/schema.json"
                )),
            )*
            "]"
        )
    };
}

/// Compile the canonical validator for one `resources/tools/<name>/schema.json`.
#[macro_export]
macro_rules! tool_validator {
    ($name:literal) => {
        ::json_validator::validator!("resources/tools/", $name, "/schema.json")
    };
}

#[derive(Clone, CopyGetters)]
pub struct Tool {
    inner: Arc<ToolInner>,
    #[getset(get_copy = "pub")]
    config: ToolConfig,
}

enum ToolInner {
    Handler(Box<dyn ErasedToolHandler>),
    Detached(Box<dyn ErasedDetachedToolHandler>),
    JsonRpc(JsonRpcTool),
}

struct JsonRpcTool {
    client: RpcClient,
    info: JsonRpcInfo,
    schema: String,
}

impl JsonRpcTool {
    fn new(client: RpcClient, info: JsonRpcInfo) -> Self {
        let schema = alloc::format!(
            r#"{{"type":"function","function":{{"name":"{}","parameters":{}}}}}"#,
            info.address(),
            info.request_schema().as_str()
        );
        Self {
            client,
            info,
            schema,
        }
    }
}

impl Tool {
    pub fn new(handler: impl ToolHandler + 'static) -> Self {
        Self {
            inner: Arc::new(ToolInner::Handler(Box::new(handler))),
            config: ToolConfig::default(),
        }
    }

    pub fn from_detached(handler: impl DetachedToolHandler + 'static) -> Self {
        Self {
            inner: Arc::new(ToolInner::Detached(Box::new(handler))),
            config: ToolConfig::default(),
        }
    }

    /// Creates a Tool backed by one registered JSON RPC endpoint.
    #[must_use]
    pub fn from_json_rpc(client: RpcClient, info: JsonRpcInfo) -> Self {
        Self {
            inner: Arc::new(ToolInner::JsonRpc(JsonRpcTool::new(client, info))),
            config: ToolConfig::default(),
        }
    }

    pub fn with_config(mut self, config: ToolConfig) -> Self {
        self.config = config;
        self
    }

    pub fn name(&self) -> &str {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.name(),
            ToolInner::Detached(handler) => handler.name(),
            ToolInner::JsonRpc(rpc) => rpc.info.address().as_ref(),
        }
    }

    pub fn schema(&self) -> &str {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.schema(),
            ToolInner::Detached(handler) => handler.schema(),
            ToolInner::JsonRpc(rpc) => &rpc.schema,
        }
    }

    pub fn usage(&self) -> Option<&str> {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.usage(),
            ToolInner::Detached(handler) => handler.usage(),
            ToolInner::JsonRpc(_) => None,
        }
    }

    pub(crate) fn classify(&self, call: &ToolInvocation) -> ToolResult<Action> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => Ok(handler.classify(call)),
            ToolInner::Detached(handler) => Ok(handler.classify(call)),
            ToolInner::JsonRpc(rpc) => {
                Ok(Action::new(rpc.info.address().as_ref(), RiskClass::High))
            }
        }
    }

    pub(crate) async fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolResult<ToolOutput> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.invoke_erased(call).await,
            ToolInner::Detached(_) => Err(ToolError::InvokeRejected(
                "dynamically detached tool requires detached execution".to_owned(),
            )
            .into()),
            ToolInner::JsonRpc(rpc) => {
                let response = rpc
                    .client
                    .call_json(rpc.info.address(), call.arguments_json())
                    .map_err(ToolError::from)?
                    .await
                    .map_err(ToolError::from)?;
                Ok(ToolOutput {
                    content: response.as_str().map_err(ToolError::from)?.to_owned(),
                    ok: true,
                })
            }
        }
    }

    pub(crate) fn is_dynamically_detached(&self) -> bool {
        matches!(self.inner.as_ref(), ToolInner::Detached(_))
    }

    pub(crate) async fn invoke_detached<'a>(
        &'a self,
        call: &'a ToolInvocation,
    ) -> ToolResult<DetachedTool> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Detached(handler) => handler.invoke_erased(call).await,
            ToolInner::Handler(_) => Err(ToolError::InvokeRejected(
                "tool does not support dynamic detached execution".to_owned(),
            )
            .into()),
            ToolInner::JsonRpc(_) => Err(ToolError::InvokeRejected(
                "tool does not support dynamic detached execution".to_owned(),
            )
            .into()),
        }
    }

    fn validate_arguments(&self, call: &ToolInvocation) -> ToolResult<()> {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler
                .arguments_validator()
                .validate_str(call.arguments_json())
                .map_err(ToolError::from)?,
            ToolInner::Detached(handler) => handler
                .arguments_validator()
                .validate_str(call.arguments_json())
                .map_err(ToolError::from)?,
            ToolInner::JsonRpc(rpc) => rpc
                .info
                .request_schema()
                .validate(call.arguments_json())
                .map_err(ToolError::from)?,
        }
        Ok(())
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Tool").field(&self.name()).finish()
    }
}
