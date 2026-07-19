//! Checkpoint interfaces for durable runtime state.

use std::{borrow::Cow, sync::{Arc, Mutex}};

type Shared<T> = Arc<Mutex<T>>;

pub type SchemaVersion = u32;
pub type PartGeneration = u32;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DurablePartMetadata {
    namespace: String,
    key: String,
}

impl DurablePartMetadata {
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateBlob<'a> {
    pub bytes: Cow<'a, [u8]>,
}

impl<'a> StateBlob<'a> {
    pub fn as_slice(&self) -> StateSlice<'_> {
        StateSlice {
            bytes: self.bytes.as_ref(),
        }
    }

    pub fn into_owned(self) -> StateBlob<'static> {
        StateBlob {
            bytes: Cow::Owned(self.bytes.into_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateSlice<'a> {
    pub bytes: &'a [u8],
}

pub trait DurableStateCodec: Sized {
    const SCHEMA_VERSION: SchemaVersion;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError>;

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableState<T> {
    value: T,
    generation: PartGeneration,
}

impl<T> DurableState<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            generation: 0,
        }
    }

    pub fn generation(&self) -> PartGeneration {
        self.generation
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.generation = self.generation.saturating_add(1);
        &mut self.value
    }

    pub fn replace(&mut self, value: T) {
        self.value = value;
        self.generation = self.generation.saturating_add(1);
    }
}

impl<T: Default> Default for DurableState<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurablePart<T> {
    metadata: DurablePartMetadata,
    state: DurableState<T>,
}

impl<T> DurablePart<T> {
    pub fn new(metadata: DurablePartMetadata, value: T) -> Self {
        Self {
            metadata,
            state: DurableState::new(value),
        }
    }

    pub fn metadata(&self) -> &DurablePartMetadata {
        &self.metadata
    }

    pub fn generation(&self) -> PartGeneration {
        self.state.generation()
    }
}

impl<T: DurableStateCodec> DurablePart<T> {
    pub fn export_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        self.state.get().encode_state()
    }

    pub(crate) fn restore(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<DurableState<T>, DurablePartError> {
        Ok(DurableState::new(T::decode_state(schema_version, state)?))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DurablePartError {
    #[error("failed to encode durable state: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("failed to decode durable state: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("invalid durable state: {0}")]
    InvalidState(&'static str),
}
