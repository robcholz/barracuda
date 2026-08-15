use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use barracuda_agent_permission::{Action, RiskClass};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::validate;
use crate::{AgentStorageError, ToolContext};

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
    arguments: serde_json::Value,
}

impl ToolInvocation {
    pub fn try_new(id: Option<&str>, name: &str, arguments_json: &str) -> ToolResult<Self> {
        let (arguments_json, arguments) = validate::normalize_arguments_json(arguments_json)?;
        Ok(Self {
            id: id.map(str::to_owned),
            name: name.to_owned(),
            arguments_json: arguments_json.to_owned(),
            arguments,
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

    pub fn arguments_value(&self) -> &serde_json::Value {
        &self.arguments
    }

    /// Deserialize the already-parsed argument object into a tool-specific DTO.
    ///
    /// This keeps JSON shape validation at the boundary without reparsing the
    /// original argument string in each handler.
    pub fn arguments<'de, T>(&'de self) -> ToolResult<T>
    where
        T: Deserialize<'de>,
    {
        T::deserialize(&self.arguments)
            .map_err(|error| ToolInvokeError::new(ToolError::InvalidArguments(error.to_string())))
    }
}

#[cfg(test)]
mod invocation_tests {
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::string::String;
    use core::cell::Cell;

    use super::{
        Tool, ToolContext, ToolError, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
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

        fn invoke<'a>(&'a self, _context: ToolContext, arguments: Self::Args) -> ToolFuture<'a> {
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
    fn invocation_keeps_one_parsed_arguments_value() -> Result<(), Box<dyn core::error::Error>> {
        let invocation =
            ToolInvocation::try_new(None, "example", r#"{"name":"lamp","enabled":true}"#)?;
        let first = invocation.arguments_value();
        let second = invocation.arguments_value();
        assert!(core::ptr::eq(first, second));
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

        let error = block_on(tool.invoke(ToolContext::stateless(), &invalid))
            .err()
            .ok_or("expected error")?;

        assert!(matches!(error.error, ToolError::ArgumentsSchema(_)));
        assert!(!invoked.get());

        let blank =
            ToolInvocation::try_new(None, "example", r#"{"name":"  \t\n","enabled":true}"#)?;
        let error = block_on(tool.invoke(ToolContext::stateless(), &blank))
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

        let output = block_on(tool.invoke(ToolContext::stateless(), &valid))?;

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
    #[error("tool invocation rejected: {0}")]
    InvokeRejected(String),
    #[error(transparent)]
    AgentStorage(#[from] AgentStorageError),
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

impl From<AgentStorageError> for ToolInvokeError {
    fn from(error: AgentStorageError) -> Self {
        Self::new(ToolError::AgentStorage(error))
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

    fn invoke<'a>(&'a self, context: ToolContext, args: Self::Args) -> ToolFuture<'a>;
}

/// A tool whose accepted and completed settlements become available at
/// different times.
pub trait DetachedToolHandler: ToolSpec {
    type Args: DeserializeOwned;

    fn invoke<'a>(&'a self, context: ToolContext, args: Self::Args) -> DetachedToolFuture<'a>;
}

trait ErasedToolHandler: ToolSpec {
    fn invoke_erased<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> ToolFuture<'a>;
}

impl<Handler> ErasedToolHandler for Handler
where
    Handler: ToolHandler,
{
    fn invoke_erased<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments::<Handler::Args>()?;
            self.invoke(context, args).await
        })
    }
}

trait ErasedDetachedToolHandler: ToolSpec {
    fn invoke_erased<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> DetachedToolFuture<'a>;
}

impl<Handler> ErasedDetachedToolHandler for Handler
where
    Handler: DetachedToolHandler,
{
    fn invoke_erased<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> DetachedToolFuture<'a> {
        Box::pin(async move {
            let args = call.arguments::<Handler::Args>()?;
            self.invoke(context, args).await
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

#[derive(Clone)]
pub struct Tool {
    inner: Arc<ToolInner>,
    config: ToolConfig,
}

enum ToolInner {
    Handler(Box<dyn ErasedToolHandler>),
    Detached(Box<dyn ErasedDetachedToolHandler>),
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

    pub fn with_config(mut self, config: ToolConfig) -> Self {
        self.config = config;
        self
    }

    pub fn config(&self) -> ToolConfig {
        self.config
    }

    pub fn name(&self) -> &str {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.name(),
            ToolInner::Detached(handler) => handler.name(),
        }
    }

    pub fn schema(&self) -> &str {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.schema(),
            ToolInner::Detached(handler) => handler.schema(),
        }
    }

    pub fn usage(&self) -> Option<&str> {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.usage(),
            ToolInner::Detached(handler) => handler.usage(),
        }
    }

    pub(crate) fn classify(&self, call: &ToolInvocation) -> ToolResult<Action> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => Ok(handler.classify(call)),
            ToolInner::Detached(handler) => Ok(handler.classify(call)),
        }
    }

    pub(crate) async fn invoke<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> ToolResult<ToolOutput> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.invoke_erased(context, call).await,
            ToolInner::Detached(_) => Err(ToolError::InvokeRejected(
                "dynamically detached tool requires detached execution".to_owned(),
            )
            .into()),
        }
    }

    pub(crate) fn is_dynamically_detached(&self) -> bool {
        matches!(self.inner.as_ref(), ToolInner::Detached(_))
    }

    pub(crate) async fn invoke_detached<'a>(
        &'a self,
        context: ToolContext,
        call: &'a ToolInvocation,
    ) -> ToolResult<DetachedTool> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Detached(handler) => handler.invoke_erased(context, call).await,
            ToolInner::Handler(_) => Err(ToolError::InvokeRejected(
                "tool does not support dynamic detached execution".to_owned(),
            )
            .into()),
        }
    }

    fn validate_arguments(&self, call: &ToolInvocation) -> ToolResult<()> {
        let validator = match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.arguments_validator(),
            ToolInner::Detached(handler) => handler.arguments_validator(),
        };
        validator
            .validate(call.arguments_value())
            .map_err(ToolError::from)?;
        Ok(())
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Tool").field(&self.name()).finish()
    }
}
