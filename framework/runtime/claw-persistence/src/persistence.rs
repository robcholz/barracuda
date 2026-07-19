use std::{
    any::{type_name, Any},
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use claw_interface::{ClawFs, FsError};

use crate::{
    DurablePart, DurablePartError, DurablePartMetadata, DurableState, DurableStateCodec,
    PartGeneration, SchemaVersion, StateBlob, StateSlice,
};

const SCHEMA_VERSION_SIZE: usize = std::mem::size_of::<SchemaVersion>();
const FILE_EXTENSION: &str = ".bin";

pub struct Persistence<Filesystem: ClawFs> {
    persistence_directory: String,
    parts: Mutex<HashMap<DurablePartMetadata, Arc<dyn RegisteredPart>>>,
    persist_lock: Mutex<()>,
    filesystem: PhantomData<Filesystem>,
}

impl<Filesystem: ClawFs> Persistence<Filesystem> {
    pub fn new(persistence_directory: impl Into<String>) -> Result<Self, PersistenceError> {
        let persistence_directory = persistence_directory.into();
        if persistence_directory.is_empty() {
            return Err(PersistenceError::EmptyDirectory);
        }

        Filesystem::create_dir_all(&persistence_directory).map_err(|source| {
            PersistenceError::CreateDirectory {
                path: persistence_directory.clone(),
                source,
            }
        })?;

        Ok(Self {
            persistence_directory,
            parts: Mutex::new(HashMap::new()),
            persist_lock: Mutex::new(()),
            filesystem: PhantomData,
        })
    }

    pub fn register<T>(&self, mut part: DurablePart<T>) -> Result<(), PersistenceError>
    where
        T: DurableStateCodec + Send + 'static,
    {
        self.validate_metadata(part.metadata())?;

        {
            let parts = lock(&self.parts);
            if parts.contains_key(part.metadata()) {
                return Err(PersistenceError::AlreadyRegistered {
                    metadata: part.metadata().clone(),
                });
            }
        }

        let path = self.part_path(part.metadata());
        let persisted_generation = match Filesystem::read(&path) {
            Ok(file) => {
                let (schema_version, state) = decode_file(&path, &file)?;
                part.state =
                    DurablePart::<T>::restore(schema_version, state).map_err(|source| {
                        PersistenceError::Part {
                            metadata: part.metadata().clone(),
                            source,
                        }
                    })?;
                Some(part.generation())
            }
            Err(FsError::NotFound) => None,
            Err(source) => {
                return Err(PersistenceError::Read { path, source });
            }
        };

        let metadata = part.metadata().clone();
        let registered: Arc<dyn RegisteredPart> = Arc::new(TypedRegisteredPart {
            part,
            persisted_generation: Mutex::new(persisted_generation),
        });

        let mut parts = lock(&self.parts);
        if parts.contains_key(&metadata) {
            return Err(PersistenceError::AlreadyRegistered { metadata });
        }
        parts.insert(metadata, registered);
        Ok(())
    }

    pub fn get<T>(
        &self,
        metadata: &DurablePartMetadata,
    ) -> Result<DurableState<T>, PersistenceError>
    where
        T: DurableStateCodec + Send + 'static,
    {
        let parts = lock(&self.parts);
        let part = parts
            .get(metadata)
            .ok_or_else(|| PersistenceError::NotRegistered {
                metadata: metadata.clone(),
            })?;

        part.state()
            .downcast_ref::<DurableState<T>>()
            .cloned()
            .ok_or_else(|| PersistenceError::TypeMismatch {
                metadata: metadata.clone(),
                expected: type_name::<T>(),
                actual: part.state_type_name(),
            })
    }

    pub fn maybe_persist(&self) -> Result<(), PersistenceError> {
        let _persist = lock(&self.persist_lock);
        let parts = {
            let parts = lock(&self.parts);
            parts
                .iter()
                .map(|(metadata, part)| (metadata.clone(), Arc::clone(part)))
                .collect::<Vec<_>>()
        };

        for (metadata, part) in parts {
            let Some(snapshot) =
                part.snapshot_if_dirty()
                    .map_err(|source| PersistenceError::Part {
                        metadata: metadata.clone(),
                        source,
                    })?
            else {
                continue;
            };

            let path = self.part_path(&metadata);
            let file = encode_file(snapshot.schema_version, snapshot.state);
            Filesystem::write_atomic(&path, &file)
                .map_err(|source| PersistenceError::Write { path, source })?;
            part.mark_persisted(snapshot.generation);
        }

        Ok(())
    }

    fn validate_metadata(&self, metadata: &DurablePartMetadata) -> Result<(), PersistenceError> {
        if let Some(namespace) = metadata.namespace() {
            if namespace.is_empty()
                || namespace.starts_with('/')
                || namespace
                    .split('/')
                    .any(|component| component.is_empty() || component == "." || component == "..")
            {
                return Err(PersistenceError::InvalidNamespace {
                    namespace: namespace.to_owned(),
                });
            }
        }

        let key = metadata.key();
        if key.is_empty() || key.contains('/') || key == "." || key == ".." {
            return Err(PersistenceError::InvalidKey {
                key: key.to_owned(),
            });
        }

        Ok(())
    }

    fn part_path(&self, metadata: &DurablePartMetadata) -> String {
        let filename = format!("{}{FILE_EXTENSION}", metadata.key());
        let relative = match metadata.namespace() {
            Some(namespace) => format!("{namespace}/{filename}"),
            None => filename,
        };

        if self.persistence_directory == "/" {
            format!("/{relative}")
        } else if self.persistence_directory.ends_with('/') {
            format!("{}{relative}", self.persistence_directory)
        } else {
            format!("{}/{relative}", self.persistence_directory)
        }
    }
}

trait RegisteredPart: Send + Sync {
    fn state(&self) -> &(dyn Any + Send + Sync);

    fn state_type_name(&self) -> &'static str;

    fn snapshot_if_dirty(&self) -> Result<Option<PartSnapshot>, DurablePartError>;

    fn mark_persisted(&self, generation: PartGeneration);
}

struct TypedRegisteredPart<T> {
    part: DurablePart<T>,
    persisted_generation: Mutex<Option<PartGeneration>>,
}

impl<T> RegisteredPart for TypedRegisteredPart<T>
where
    T: DurableStateCodec + Send + 'static,
{
    fn state(&self) -> &(dyn Any + Send + Sync) {
        &self.part.state
    }

    fn state_type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn snapshot_if_dirty(&self) -> Result<Option<PartSnapshot>, DurablePartError> {
        let persisted_generation = *lock(&self.persisted_generation);
        if persisted_generation == Some(self.part.generation()) {
            return Ok(None);
        }

        let (generation, schema_version, state) = self.part.export_state()?;
        if persisted_generation == Some(generation) {
            return Ok(None);
        }

        Ok(Some(PartSnapshot {
            generation,
            schema_version,
            state,
        }))
    }

    fn mark_persisted(&self, generation: PartGeneration) {
        *lock(&self.persisted_generation) = Some(generation);
    }
}

struct PartSnapshot {
    generation: PartGeneration,
    schema_version: SchemaVersion,
    state: StateBlob<'static>,
}

fn encode_file(schema_version: SchemaVersion, state: StateBlob<'_>) -> Vec<u8> {
    let mut file = Vec::with_capacity(SCHEMA_VERSION_SIZE + state.bytes.len());
    file.extend_from_slice(&schema_version.to_le_bytes());
    file.extend_from_slice(state.bytes.as_ref());
    file
}

fn decode_file<'a>(
    path: &str,
    file: &'a [u8],
) -> Result<(SchemaVersion, StateSlice<'a>), PersistenceError> {
    if file.len() < SCHEMA_VERSION_SIZE {
        return Err(PersistenceError::TruncatedFile {
            path: path.to_owned(),
            actual_size: file.len(),
        });
    }

    let schema_version = SchemaVersion::from_le_bytes([file[0], file[1], file[2], file[3]]);
    Ok((
        schema_version,
        StateSlice {
            bytes: &file[SCHEMA_VERSION_SIZE..],
        },
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("persistence directory cannot be empty")]
    EmptyDirectory,
    #[error("invalid persistence namespace `{namespace}`")]
    InvalidNamespace { namespace: String },
    #[error("invalid persistence key `{key}`")]
    InvalidKey { key: String },
    #[error("durable part is already registered: {metadata:?}")]
    AlreadyRegistered { metadata: DurablePartMetadata },
    #[error("durable part is not registered: {metadata:?}")]
    NotRegistered { metadata: DurablePartMetadata },
    #[error(
        "durable part type mismatch for {metadata:?}: requested {expected}, registered {actual}"
    )]
    TypeMismatch {
        metadata: DurablePartMetadata,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("failed to create persistence directory `{path}`: {source}")]
    CreateDirectory {
        path: String,
        #[source]
        source: FsError,
    },
    #[error("failed to read persistence file `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: FsError,
    },
    #[error("failed to write persistence file `{path}`: {source}")]
    Write {
        path: String,
        #[source]
        source: FsError,
    },
    #[error(
        "persistence file `{path}` is too short: expected at least 4 bytes, found {actual_size}"
    )]
    TruncatedFile { path: String, actual_size: usize },
    #[error("failed to process durable part {metadata:?}: {source}")]
    Part {
        metadata: DurablePartMetadata,
        #[source]
        source: DurablePartError,
    },
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use claw_interface::{ClawFs, MemFs};

    use super::*;

    #[derive(Debug)]
    struct TestState {
        value: u32,
    }

    impl DurableStateCodec for TestState {
        const SCHEMA_VERSION: SchemaVersion = 7;

        fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
            Ok(StateBlob {
                bytes: Cow::Owned(self.value.to_le_bytes().to_vec()),
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
            if state.bytes.len() != std::mem::size_of::<u32>() {
                return Err(DurablePartError::InvalidState(
                    "invalid test state payload size",
                ));
            }
            Ok(Self {
                value: u32::from_le_bytes([
                    state.bytes[0],
                    state.bytes[1],
                    state.bytes[2],
                    state.bytes[3],
                ]),
            })
        }
    }

    #[derive(Debug)]
    struct OtherState;

    impl DurableStateCodec for OtherState {
        const SCHEMA_VERSION: SchemaVersion = 1;

        fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
            Ok(StateBlob {
                bytes: Cow::Borrowed(b"other"),
            })
        }

        fn decode_state(
            _schema_version: SchemaVersion,
            _state: StateSlice<'_>,
        ) -> Result<Self, DurablePartError> {
            Ok(Self)
        }
    }

    #[test]
    fn register_get_persist_and_restore_root_state() {
        let root = "/claw-persistence-root-state";
        let metadata = DurablePartMetadata::new(None, "state");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .register(DurablePart::new(metadata.clone(), TestState { value: 1 }))
            .expect("part registers");

        let state = persistence
            .get::<TestState>(&metadata)
            .expect("registered state is available");
        state.get_mut().value = 2;
        persistence.maybe_persist().expect("dirty state persists");

        let file = MemFs::read(&format!("{root}/state.bin")).expect("root state file exists");
        assert_eq!(&file[..SCHEMA_VERSION_SIZE], &7_u32.to_le_bytes());
        assert_eq!(&file[SCHEMA_VERSION_SIZE..], &2_u32.to_le_bytes());

        let restored = Persistence::<MemFs>::new(root).expect("persistence reinitializes");
        restored
            .register(DurablePart::new(metadata.clone(), TestState { value: 99 }))
            .expect("persisted part restores while registering");
        assert_eq!(
            restored
                .get::<TestState>(&metadata)
                .expect("restored state is available")
                .get()
                .value,
            2
        );
    }

    #[test]
    fn namespace_and_key_map_to_framework_bin_path() {
        let root = "/claw-persistence-namespaced-state";
        let metadata = DurablePartMetadata::new(Some("sessions/session-7".to_owned()), "state");
        assert_eq!(metadata.key(), "state");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .register(DurablePart::new(metadata, TestState { value: 7 }))
            .expect("part registers");
        persistence.maybe_persist().expect("new part persists");

        assert!(MemFs::exists(&format!(
            "{root}/sessions/session-7/state.bin"
        )));
    }

    #[test]
    fn get_rejects_an_unregistered_type() {
        let metadata = DurablePartMetadata::new(None, "state");
        let persistence = Persistence::<MemFs>::new("/claw-persistence-type-check")
            .expect("persistence initializes");
        persistence
            .register(DurablePart::new(metadata.clone(), TestState { value: 1 }))
            .expect("part registers");

        assert!(matches!(
            persistence.get::<OtherState>(&metadata),
            Err(PersistenceError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn register_rejects_a_truncated_schema_version() {
        let root = "/claw-persistence-truncated-version";
        let path = format!("{root}/state.bin");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        MemFs::write_atomic(&path, &[1, 0, 0]).expect("truncated file is installed");

        let error = persistence
            .register(DurablePart::new(
                DurablePartMetadata::new(None, "state"),
                TestState { value: 1 },
            ))
            .expect_err("truncated schema version is rejected");

        assert!(matches!(
            error,
            PersistenceError::TruncatedFile { actual_size: 3, .. }
        ));
    }
}
