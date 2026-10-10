use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use barracuda_agent_permission::{Action, RiskClass};
use portable_atomic_util::Arc;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::background::{BackgroundTool, BackgroundToolFuture};
use super::validate;

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult<ToolOutput>> + 'a>>;
pub type ToolResult<T> = Result<T, ToolInvokeError>;

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

/// A Tool whose call keeps running in the Agent's background pool after its
/// accepted output returns to the model turn.
pub trait BackgroundToolHandler: ToolSpec {
    type Args: DeserializeOwned;

    fn invoke<'a>(&'a self, args: Self::Args) -> BackgroundToolFuture<'a>;
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

trait ErasedBackgroundToolHandler: ToolSpec {
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> BackgroundToolFuture<'a>;
}

impl<Handler> ErasedBackgroundToolHandler for Handler
where
    Handler: BackgroundToolHandler,
{
    fn invoke_erased<'a>(&'a self, call: &'a ToolInvocation) -> BackgroundToolFuture<'a> {
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

/// One registered Tool: an ordinary Tool whose call settles in the model turn,
/// or a background Tool whose call continues in the Agent's background pool.
#[derive(Clone)]
pub struct Tool {
    inner: Arc<ToolInner>,
}

enum ToolInner {
    Handler(Box<dyn ErasedToolHandler>),
    Background(Box<dyn ErasedBackgroundToolHandler>),
}

impl Tool {
    pub fn new(handler: impl ToolHandler + 'static) -> Self {
        Self {
            inner: Arc::new(ToolInner::Handler(Box::new(handler))),
        }
    }

    pub fn background(handler: impl BackgroundToolHandler + 'static) -> Self {
        Self {
            inner: Arc::new(ToolInner::Background(Box::new(handler))),
        }
    }

    fn spec(&self) -> &dyn ToolSpec {
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => &**handler,
            ToolInner::Background(handler) => &**handler,
        }
    }

    pub fn name(&self) -> &str {
        self.spec().name()
    }

    pub fn schema(&self) -> &str {
        self.spec().schema()
    }

    pub fn usage(&self) -> Option<&str> {
        self.spec().usage()
    }

    pub(crate) fn classify(&self, call: &ToolInvocation) -> ToolResult<Action> {
        self.validate_arguments(call)?;
        Ok(self.spec().classify(call))
    }

    pub(crate) fn is_background(&self) -> bool {
        matches!(self.inner.as_ref(), ToolInner::Background(_))
    }

    pub(crate) async fn invoke<'a>(&'a self, call: &'a ToolInvocation) -> ToolResult<ToolOutput> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Handler(handler) => handler.invoke_erased(call).await,
            ToolInner::Background(_) => Err(ToolError::InvokeRejected(
                "background tool requires a background pool".to_owned(),
            )
            .into()),
        }
    }

    pub(crate) async fn invoke_background<'a>(
        &'a self,
        call: &'a ToolInvocation,
    ) -> ToolResult<BackgroundTool> {
        self.validate_arguments(call)?;
        match self.inner.as_ref() {
            ToolInner::Background(handler) => handler.invoke_erased(call).await,
            ToolInner::Handler(_) => Err(ToolError::InvokeRejected(
                "tool does not run in the background".to_owned(),
            )
            .into()),
        }
    }

    fn validate_arguments(&self, call: &ToolInvocation) -> ToolResult<()> {
        self.spec()
            .arguments_validator()
            .validate_str(call.arguments_json())
            .map_err(ToolError::from)?;
        Ok(())
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Tool").field(&self.name()).finish()
    }
}
