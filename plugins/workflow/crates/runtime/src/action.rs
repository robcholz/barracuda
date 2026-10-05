//! Typed Plugin capability for direct Workflow Actions.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use getset::{CopyGetters, Getters};
use json_validator::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::WorkflowValue;

/// Stable address of one Workflow Action.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkflowActionAddress(String);

impl WorkflowActionAddress {
    /// Returns this Action address as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for WorkflowActionAddress {
    type Error = WorkflowActionAddressError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(String::from(value))
    }
}

impl TryFrom<String> for WorkflowActionAddress {
    type Error = WorkflowActionAddressError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(WorkflowActionAddressError::Empty);
        }
        for (index, character) in value.char_indices() {
            let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.');
            if !valid {
                return Err(WorkflowActionAddressError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }
}

impl fmt::Display for WorkflowActionAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while parsing a [`WorkflowActionAddress`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowActionAddressError {
    /// Action addresses cannot be empty.
    #[error("Workflow Action address cannot be empty")]
    Empty,
    /// Action addresses contain only ASCII letters, digits, `_`, `-`, and `.`.
    #[error("invalid Workflow Action address character {character:?} at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
        /// Invalid character found in the input.
        character: char,
    },
}

/// Static request and response contract for one Workflow Action.
#[derive(Clone, Copy, Debug, PartialEq, CopyGetters)]
pub struct WorkflowActionSchema {
    /// Stable Action address referenced by the Workflow DSL.
    #[getset(get_copy = "pub")]
    address: &'static str,
    /// Static request JSON Schema and validator.
    #[getset(get_copy = "pub")]
    request: JsonSchema,
    /// Static response JSON Schema and validator.
    #[getset(get_copy = "pub")]
    response: JsonSchema,
}

impl WorkflowActionSchema {
    /// Creates a static Action contract.
    #[must_use]
    pub const fn new(address: &'static str, request: JsonSchema, response: JsonSchema) -> Self {
        Self {
            address,
            request,
            response,
        }
    }
}

/// Includes conventional request and response schemas for one Workflow Action.
///
/// The calling crate resolves both documents below
/// `../../schemas/action/<address>/`.
#[macro_export]
macro_rules! workflow_action_schema {
    ($address:literal $(,)?) => {
        $crate::WorkflowActionSchema::new(
            $address,
            $crate::__private::json_validator::json_schema!(
                "../../schemas/action/",
                $address,
                "/request.json"
            ),
            $crate::__private::json_validator::json_schema!(
                "../../schemas/action/",
                $address,
                "/response.json"
            ),
        )
    };
}

/// Compiles inline request and response schemas for a Workflow Action.
#[macro_export]
macro_rules! workflow_action_schema_inline {
    ($address:literal, $request:literal, $response:literal $(,)?) => {{
        const REQUEST: $crate::__private::json_validator::JsonSchema =
            $crate::__private::json_validator::json_schema_inline!($request);
        const RESPONSE: $crate::__private::json_validator::JsonSchema =
            $crate::__private::json_validator::json_schema_inline!($response);
        $crate::WorkflowActionSchema::new($address, REQUEST, RESPONSE)
    }};
}

/// Static metadata used to discover and link one registered Workflow Action.
#[derive(Clone, Debug, Getters, CopyGetters, PartialEq)]
pub struct WorkflowActionDescriptor {
    /// Stable Action address referenced by the Workflow DSL.
    #[getset(get = "pub")]
    address: WorkflowActionAddress,
    /// Static request JSON Schema and validator.
    #[getset(get_copy = "pub")]
    request_schema: JsonSchema,
    /// Static response JSON Schema and validator.
    #[getset(get_copy = "pub")]
    response_schema: JsonSchema,
    request_shape: WorkflowValue,
    response_shape: WorkflowValue,
}

impl WorkflowActionDescriptor {
    fn try_from_schema(schema: WorkflowActionSchema) -> Result<Self, WorkflowActionRegistryError> {
        let address = WorkflowActionAddress::try_from(schema.address())
            .map_err(WorkflowActionRegistryError::InvalidAddress)?;
        let request_shape = serde_json::from_str(schema.request().as_str())
            .map_err(|_error| WorkflowActionRegistryError::InvalidRequestSchema(address.clone()))?;
        let response_shape =
            serde_json::from_str(schema.response().as_str()).map_err(|_error| {
                WorkflowActionRegistryError::InvalidResponseSchema(address.clone())
            })?;
        Ok(Self {
            address,
            request_schema: schema.request(),
            response_schema: schema.response(),
            request_shape,
            response_shape,
        })
    }

    pub(crate) fn request_shape(&self) -> &WorkflowValue {
        &self.request_shape
    }

    pub(crate) fn response_shape(&self) -> &WorkflowValue {
        &self.response_shape
    }
}

/// Boxed local future returned by a [`WorkflowActionHandler`].
pub type WorkflowActionFuture<'a, Response> =
    Pin<Box<dyn Future<Output = Result<Response, WorkflowActionError>> + 'a>>;

/// Typed operation registered as one Workflow Action.
pub trait WorkflowActionHandler: 'static {
    /// Request decoded with Serde after schema validation.
    type Request: DeserializeOwned;
    /// Response encoded with Serde before schema validation.
    type Response: Serialize;

    /// Static address, request schema, response schema, and validators.
    const SCHEMA: WorkflowActionSchema;

    /// Executes the Action with its typed request.
    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response>;
}

pub(crate) trait ErasedWorkflowAction {
    fn descriptor(&self) -> &WorkflowActionDescriptor;
    fn invoke_erased(&self, input: WorkflowValue) -> WorkflowActionFuture<'_, WorkflowValue>;
}

struct TypedWorkflowAction<Handler> {
    descriptor: WorkflowActionDescriptor,
    handler: Handler,
}

impl<Handler> ErasedWorkflowAction for TypedWorkflowAction<Handler>
where
    Handler: WorkflowActionHandler,
{
    fn descriptor(&self) -> &WorkflowActionDescriptor {
        &self.descriptor
    }

    fn invoke_erased(&self, input: WorkflowValue) -> WorkflowActionFuture<'_, WorkflowValue> {
        Box::pin(async move {
            Handler::SCHEMA
                .request()
                .validate(&input)
                .map_err(|error| {
                    WorkflowActionError::new(format!("invalid Workflow Action request: {error}"))
                })?;
            let request = serde_json::from_value::<Handler::Request>(input).map_err(|error| {
                WorkflowActionError::new(format!(
                    "failed to decode Workflow Action request: {error}"
                ))
            })?;
            let response = self.handler.invoke(request).await?;
            let response = serde_json::to_value(response).map_err(|error| {
                WorkflowActionError::new(format!(
                    "failed to encode Workflow Action response: {error}"
                ))
            })?;
            Handler::SCHEMA
                .response()
                .validate(&response)
                .map_err(|error| {
                    WorkflowActionError::new(format!("invalid Workflow Action response: {error}"))
                })?;
            Ok(response)
        })
    }
}

/// Stable failure returned by a direct Workflow Action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkflowActionError {
    message: String,
}

impl WorkflowActionError {
    /// Creates an Action failure with caller-facing context.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

struct RegisteredAction {
    registration_id: usize,
    action: Rc<dyn ErasedWorkflowAction>,
}

struct RegistryState {
    next_registration_id: Cell<usize>,
    actions: RefCell<Vec<RegisteredAction>>,
}

/// Typed capability through which Plugins add Workflow Actions.
#[derive(Clone)]
pub struct WorkflowActionRegistry {
    state: Rc<RegistryState>,
}

impl WorkflowActionRegistry {
    /// Creates an empty Action registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Rc::new(RegistryState {
                next_registration_id: Cell::new(0),
                actions: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Adds one typed Action until the returned registration is dropped.
    pub fn add_action<Handler>(
        &self,
        handler: Handler,
    ) -> Result<WorkflowActionRegistration, WorkflowActionRegistryError>
    where
        Handler: WorkflowActionHandler,
    {
        let descriptor = WorkflowActionDescriptor::try_from_schema(Handler::SCHEMA)?;
        let address = descriptor.address().clone();
        let mut actions = self.state.actions.borrow_mut();
        if actions
            .iter()
            .any(|registered| registered.action.descriptor().address() == &address)
        {
            return Err(WorkflowActionRegistryError::DuplicateAddress(address));
        }
        let registration_id = self.state.next_registration_id.get();
        self.state
            .next_registration_id
            .set(registration_id.saturating_add(1));
        actions.push(RegisteredAction {
            registration_id,
            action: Rc::new(TypedWorkflowAction {
                descriptor,
                handler,
            }),
        });
        Ok(WorkflowActionRegistration {
            state: Rc::downgrade(&self.state),
            registration_id,
        })
    }

    /// Returns metadata for every Action in registration order.
    #[must_use]
    pub fn descriptors(&self) -> Vec<WorkflowActionDescriptor> {
        self.state
            .actions
            .borrow()
            .iter()
            .map(|registered| registered.action.descriptor().clone())
            .collect()
    }

    pub(crate) fn resolve(
        &self,
        address: &WorkflowActionAddress,
    ) -> Option<Rc<dyn ErasedWorkflowAction>> {
        self.state
            .actions
            .borrow()
            .iter()
            .find(|registered| registered.action.descriptor().address() == address)
            .map(|registered| Rc::clone(&registered.action))
    }
}

impl Default for WorkflowActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure returned while adding a Workflow Action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowActionRegistryError {
    /// The Action schema declares an invalid address.
    #[error("invalid Workflow Action address: {0}")]
    InvalidAddress(WorkflowActionAddressError),
    /// The Action's static request schema was not valid JSON.
    #[error("Workflow Action request schema is invalid: {0}")]
    InvalidRequestSchema(WorkflowActionAddress),
    /// The Action's static response schema was not valid JSON.
    #[error("Workflow Action response schema is invalid: {0}")]
    InvalidResponseSchema(WorkflowActionAddress),
    /// Another registered Action already owns this address.
    #[error("Workflow Action is already registered: {0}")]
    DuplicateAddress(WorkflowActionAddress),
}

/// Scoped Action registration removed when its owner Plugin unloads.
pub struct WorkflowActionRegistration {
    state: Weak<RegistryState>,
    registration_id: usize,
}

impl Drop for WorkflowActionRegistration {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .actions
            .borrow_mut()
            .retain(|registered| registered.registration_id != self.registration_id);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use alloc::{boxed::Box, string::String};

    use futures_lite::future::block_on;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::{
        WorkflowActionAddress, WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistry,
        WorkflowActionSchema,
    };
    #[derive(Deserialize)]
    struct EchoRequest {
        message: String,
    }

    #[derive(Serialize)]
    struct EchoResponse {
        message: String,
    }

    struct Echo;

    impl WorkflowActionHandler for Echo {
        type Request = EchoRequest;
        type Response = EchoResponse;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
            "test.echo",
            r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}"#,
            r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}"#,
        );

        fn invoke(&self, request: EchoRequest) -> WorkflowActionFuture<'_, EchoResponse> {
            Box::pin(async move {
                Ok(EchoResponse {
                    message: request.message,
                })
            })
        }
    }

    #[test]
    fn typed_handler_uses_static_validation_and_serde_conversion() {
        block_on(async {
            let registry = WorkflowActionRegistry::new();
            let _registration = registry.add_action(Echo).expect("register typed Action");
            let address = WorkflowActionAddress::try_from("test.echo").expect("valid address");
            let action = registry.resolve(&address).expect("resolve typed Action");

            let response = action
                .invoke_erased(json!({ "message": "hello" }))
                .await
                .expect("invoke typed Action");
            assert_eq!(response, json!({ "message": "hello" }));

            let error = action
                .invoke_erased(json!({ "message": 7 }))
                .await
                .expect_err("reject request before Serde conversion");
            assert!(error.message().contains("invalid Workflow Action request"));
        });
    }
}
