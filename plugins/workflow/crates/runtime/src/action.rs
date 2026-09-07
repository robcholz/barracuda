//! Typed Plugin capability for direct Workflow Actions.

use alloc::boxed::Box;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::fmt;
use core::future::Future;
use core::pin::Pin;

use getset::Getters;

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

/// Static metadata used to validate and discover one Workflow Action.
#[derive(Clone, Debug, Getters, PartialEq)]
pub struct WorkflowActionDescriptor {
    /// Stable Action address referenced by the Workflow DSL.
    #[getset(get = "pub")]
    address: WorkflowActionAddress,
    /// Human-readable Action description.
    #[getset(get = "pub")]
    description: String,
    /// JSON Schema for the Action input.
    #[getset(get = "pub")]
    request_schema: WorkflowValue,
    /// JSON Schema for the Action output.
    #[getset(get = "pub")]
    response_schema: WorkflowValue,
}

impl WorkflowActionDescriptor {
    /// Creates Action metadata from an address and JSON schemas.
    #[must_use]
    pub fn new(
        address: WorkflowActionAddress,
        description: impl Into<String>,
        request_schema: WorkflowValue,
        response_schema: WorkflowValue,
    ) -> Self {
        Self {
            address,
            description: description.into(),
            request_schema,
            response_schema,
        }
    }
}

/// Boxed local future returned by a [`WorkflowAction`].
pub type WorkflowActionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkflowValue, WorkflowActionError>> + 'a>>;

/// Direct operation that may be placed in a Workflow definition.
pub trait WorkflowAction: 'static {
    /// Returns the Action metadata.
    fn descriptor(&self) -> &WorkflowActionDescriptor;

    /// Invokes the Action with one owned JSON value.
    fn invoke(&self, input: WorkflowValue) -> WorkflowActionFuture<'_>;
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
    action: Rc<dyn WorkflowAction>,
}

struct RegistryState {
    next_registration_id: Cell<usize>,
    actions: RefCell<Vec<RegisteredAction>>,
}

/// Typed capability through which Plugins register Workflow Actions.
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

    /// Registers one Action until the returned guard is dropped.
    pub fn register<A>(
        &self,
        action: A,
    ) -> Result<WorkflowActionRegistration, WorkflowActionRegistryError>
    where
        A: WorkflowAction,
    {
        let address = action.descriptor().address().clone();
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
            action: Rc::new(action),
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
    ) -> Option<Rc<dyn WorkflowAction>> {
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

/// Failure returned while registering a Workflow Action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowActionRegistryError {
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
