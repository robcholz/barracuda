//! Persistence primitives for durable runtime state.

mod persistence;

pub use persistence::{Persistence, PersistenceError};

use std::{
    borrow::Cow,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

type Shared<T> = Arc<Mutex<T>>;

pub type SchemaVersion = u32;
pub type PartGeneration = u32;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DurablePartMetadata {
    namespace: Option<String>,
    key: String,
}

impl DurablePartMetadata {
    pub fn new(namespace: Option<String>, key: impl Into<String>) -> Self {
        Self {
            namespace,
            key: key.into(),
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
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

/// Encodes and decodes the opaque binary content of one durable state.
///
/// [`Persistence`] stores `SCHEMA_VERSION` separately from these bytes and
/// passes the stored version to `decode_state`.
pub trait DurableStateCodec: Sized {
    const SCHEMA_VERSION: SchemaVersion;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError>;

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError>;
}

#[derive(Debug)]
pub struct DurableState<T> {
    inner: Shared<DurableStateInner<T>>,
}

#[derive(Debug)]
struct DurableStateInner<T> {
    value: T,
    generation: PartGeneration,
}

struct StateGuard<'a, T>(MutexGuard<'a, DurableStateInner<T>>);

impl<T> Deref for StateGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

struct StateGuardMut<'a, T>(MutexGuard<'a, DurableStateInner<T>>);

impl<T> Deref for StateGuardMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<T> DerefMut for StateGuardMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.value
    }
}

impl<T> Clone for DurableState<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> DurableState<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DurableStateInner {
                value,
                generation: 0,
            })),
        }
    }

    pub fn generation(&self) -> PartGeneration {
        self.lock().generation
    }

    pub fn get(&self) -> impl Deref<Target = T> + '_ {
        StateGuard(self.lock())
    }

    pub fn get_mut(&self) -> impl DerefMut<Target = T> + '_ {
        let mut state = self.lock();
        state.generation = state.generation.saturating_add(1);
        StateGuardMut(state)
    }

    pub fn replace(&self, value: T) {
        let mut state = self.lock();
        state.value = value;
        state.generation = state.generation.saturating_add(1);
    }

    fn lock(&self) -> MutexGuard<'_, DurableStateInner<T>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: Default> Default for DurableState<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[derive(Debug)]
pub struct DurablePart<T> {
    metadata: DurablePartMetadata,
    state: DurableState<T>,
}

impl<T> Clone for DurablePart<T> {
    fn clone(&self) -> Self {
        Self {
            metadata: self.metadata.clone(),
            state: self.state.clone(),
        }
    }
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
    pub(crate) fn export_state(
        &self,
    ) -> Result<(PartGeneration, SchemaVersion, StateBlob<'static>), DurablePartError> {
        let state = self.state.lock();
        let blob = state.value.encode_state()?.into_owned();

        Ok((state.generation, T::SCHEMA_VERSION, blob))
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    struct TestState {
        value: u32,
    }

    impl DurableStateCodec for TestState {
        const SCHEMA_VERSION: SchemaVersion = 7;

        fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
            Ok(StateBlob {
                bytes: Cow::Owned(serde_json::to_vec(self).map_err(DurablePartError::Encode)?),
            })
        }

        fn decode_state(
            schema_version: SchemaVersion,
            state: StateSlice<'_>,
        ) -> Result<Self, DurablePartError> {
            if schema_version != Self::SCHEMA_VERSION {
                return Err(DurablePartError::InvalidState(
                    "unsupported test state schema",
                ));
            }
            serde_json::from_slice(state.bytes).map_err(DurablePartError::Decode)
        }
    }

    #[test]
    fn clones_share_state_across_threads() {
        let state = DurableState::new(TestState { value: 1 });
        let cloned = state.clone();

        std::thread::spawn(move || {
            let mut state = cloned.get_mut();
            state.value = 2;
        })
        .join()
        .expect("state mutation thread completes");

        let value = state.get().value;
        assert_eq!(value, 2);
        assert_eq!(state.generation(), 1);
    }

    #[test]
    fn export_captures_one_owned_snapshot() {
        let part = DurablePart::new(
            DurablePartMetadata::new(Some("test".to_owned()), "state"),
            TestState { value: 3 },
        );
        part.state.get_mut().value = 4;

        let (generation, schema_version, blob) =
            part.export_state().expect("state export succeeds");

        assert_eq!(generation, 1);
        assert_eq!(schema_version, TestState::SCHEMA_VERSION);
        assert_eq!(blob.bytes.as_ref(), br#"{"value":4}"#);
    }
}
