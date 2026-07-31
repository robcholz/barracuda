//! Persistence primitives for runtime-owned durable state.
//!
//! Callers open typed singleton or collection entries, decode persisted DTOs,
//! construct their own [`DurableState`], and register it for observation by
//! [`Persistence::maybe_persist`]. Normal registrations are non-owning.

mod persistence;

pub use persistence::{Collection, Persistence, PersistenceError, Singleton};

use std::{
    borrow::Cow,
    error::Error,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
};

type Shared<T> = Arc<Mutex<T>>;

pub type SharedPersistence<Filesystem> = Arc<Persistence<Filesystem>>;

pub type SchemaVersion = u32;
type PartGeneration = u64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(String);

impl InstanceId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidInstanceId> {
        let id = id.into();
        if is_valid_key(&id) {
            Ok(Self(id))
        } else {
            Err(InvalidInstanceId { id })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid collection instance id `{id}`")]
pub struct InvalidInstanceId {
    id: String,
}

impl InvalidInstanceId {
    pub fn as_str(&self) -> &str {
        &self.id
    }
}

pub(crate) fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['/', '\\', '\0']) && key != "." && key != ".."
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

/// An encoded snapshot of runtime-owned durable state.
pub(crate) struct DurableStateSnapshot {
    generation: PartGeneration,
    schema_version: SchemaVersion,
    state: StateBlob<'static>,
}

impl DurableStateSnapshot {
    fn new(generation: u64, schema_version: SchemaVersion, state: StateBlob<'static>) -> Self {
        Self {
            generation,
            schema_version,
            state,
        }
    }

    pub(crate) fn into_parts(self) -> (PartGeneration, SchemaVersion, StateBlob<'static>) {
        (self.generation, self.schema_version, self.state)
    }
}

#[derive(Debug)]
pub struct DurableState<T> {
    inner: Shared<DurableStateInner<T>>,
}

#[derive(Debug)]
pub(crate) struct WeakDurableState<T> {
    inner: Weak<Mutex<DurableStateInner<T>>>,
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

impl<T> Clone for WeakDurableState<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Weak::clone(&self.inner),
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

    #[cfg(test)]
    pub(crate) fn generation(&self) -> PartGeneration {
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

    fn downgrade(&self) -> WeakDurableState<T> {
        WeakDurableState {
            inner: Arc::downgrade(&self.inner),
        }
    }

    fn lock(&self) -> MutexGuard<'_, DurableStateInner<T>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T> WeakDurableState<T> {
    pub(crate) fn generation(&self) -> Option<PartGeneration> {
        let inner = self.inner.upgrade()?;
        let generation = inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation;
        Some(generation)
    }
}

impl<T> WeakDurableState<T>
where
    T: DurableStateCodec + Send + 'static,
{
    pub(crate) fn snapshot(&self) -> Result<Option<DurableStateSnapshot>, DurablePartError> {
        let Some(inner) = self.inner.upgrade() else {
            return Ok(None);
        };
        let state = inner.lock().unwrap_or_else(PoisonError::into_inner);
        let blob = state.value.encode_state()?.into_owned();
        Ok(Some(DurableStateSnapshot::new(
            state.generation,
            T::SCHEMA_VERSION,
            blob,
        )))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DurablePartError {
    #[error("failed to encode durable state: {0}")]
    Encode(#[source] Box<dyn Error + Send + Sync + 'static>),
    #[error("failed to decode durable state: {0}")]
    Decode(#[source] Box<dyn Error + Send + Sync + 'static>),
    #[error("invalid durable state: {0}")]
    InvalidState(&'static str),
}

impl DurablePartError {
    pub fn encode(source: impl Error + Send + Sync + 'static) -> Self {
        Self::Encode(Box::new(source))
    }

    pub fn decode(source: impl Error + Send + Sync + 'static) -> Self {
        Self::Decode(Box::new(source))
    }
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
        const SCHEMA_VERSION: SchemaVersion = 1;

        fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
            Ok(StateBlob {
                bytes: Cow::Owned(serde_json::to_vec(self).map_err(DurablePartError::encode)?),
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
            serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
        }
    }

    #[test]
    fn instance_id_is_validated_when_constructed() {
        assert_eq!(
            InstanceId::new("session-1")
                .expect("valid instance id is accepted")
                .as_str(),
            "session-1"
        );

        for invalid in [
            "",
            ".",
            "..",
            "nested/session",
            "nested\\session",
            "nul\0key",
        ] {
            let error = InstanceId::new(invalid).expect_err("invalid instance id is rejected");
            assert_eq!(error.as_str(), invalid);
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
        let state = DurableState::new(TestState { value: 3 });
        state.get_mut().value = 4;

        let (generation, schema_version, blob) = state
            .downgrade()
            .snapshot()
            .expect("state export succeeds")
            .expect("state owner remains alive")
            .into_parts();

        assert_eq!(generation, 1);
        assert_eq!(schema_version, TestState::SCHEMA_VERSION);
        assert_eq!(blob.bytes.as_ref(), br#"{"value":4}"#);
    }
}
