use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{btree_map::Entry as MapEntry, BTreeMap};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::{type_name, TypeId};
use core::cell::RefCell;
use core::error::Error;
use core::marker::PhantomData;

use barracuda_vfs::{FsError, ScopedVfs};

use crate::{
    is_valid_key, DurablePartError, DurableState, DurableStateCodec, InstanceId, PartGeneration,
    SchemaVersion, StateBlob, StateSlice, WeakDurableState,
};

const SCHEMA_VERSION_SIZE: usize = core::mem::size_of::<SchemaVersion>();
const FILE_EXTENSION: &str = ".bin";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum EntryKey {
    Singleton(String),
    Collection(String),
}

impl EntryKey {
    fn name(&self) -> &str {
        match self {
            Self::Singleton(name) | Self::Collection(name) => name,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Singleton(_) => "singleton",
            Self::Collection(_) => "collection",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum StateAddress {
    Singleton {
        name: String,
    },
    Collection {
        name: String,
        instance_id: InstanceId,
    },
}

/// Filesystem-backed registry for typed durable state.
pub struct Persistence {
    persistence_directory: String,
    entry_types: RefCell<BTreeMap<EntryKey, RegisteredEntryType>>,
    parts: RefCell<BTreeMap<StateAddress, Arc<dyn RegisteredPart>>>,
    removals: RefCell<Vec<(StateAddress, String)>>,
    filesystem: ScopedVfs,
}

/// One typed singleton entry.
pub struct Singleton<'a, T> {
    persistence: &'a Persistence,
    name: String,
    state: PhantomData<fn() -> T>,
}

/// One typed collection entry.
pub struct Collection<'a, T> {
    persistence: &'a Persistence,
    name: String,
    state: PhantomData<fn() -> T>,
}

impl Persistence {
    /// Create a persistence registry rooted at `persistence_directory`.
    pub async fn new(
        filesystem: ScopedVfs,
        persistence_directory: impl Into<String>,
    ) -> Result<Self, PersistenceError> {
        let persistence_directory = persistence_directory.into();
        if persistence_directory.trim().is_empty() {
            return Err(PersistenceError::EmptyDirectory);
        }

        filesystem
            .create_dir_all(&persistence_directory)
            .await
            .map_err(|source| {
                PersistenceError::storage(
                    "create persistence directory",
                    persistence_directory.clone(),
                    source,
                )
            })?;

        Ok(Self {
            persistence_directory,
            entry_types: RefCell::new(BTreeMap::new()),
            parts: RefCell::new(BTreeMap::new()),
            removals: RefCell::new(Vec::new()),
            filesystem,
        })
    }

    /// Open a typed singleton entry.
    pub fn singleton<T>(
        &self,
        name: impl Into<String>,
    ) -> Result<Singleton<'_, T>, PersistenceError>
    where
        T: DurableStateCodec + 'static,
    {
        let name = name.into();
        self.ensure_entry_type::<T>(&EntryKey::Singleton(name.clone()))?;
        Ok(Singleton {
            persistence: self,
            name,
            state: PhantomData,
        })
    }

    /// Open a typed collection entry.
    pub fn collection<T>(
        &self,
        name: impl Into<String>,
    ) -> Result<Collection<'_, T>, PersistenceError>
    where
        T: DurableStateCodec + 'static,
    {
        let name = name.into();
        self.ensure_entry_type::<T>(&EntryKey::Collection(name.clone()))?;
        Ok(Collection {
            persistence: self,
            name,
            state: PhantomData,
        })
    }

    /// Persist every registered state whose generation changed.
    pub async fn maybe_persist(&self) -> Result<(), PersistenceError> {
        let removals = core::mem::take(&mut *self.removals.borrow_mut());
        for (index, (address, path)) in removals.iter().enumerate() {
            if let Err(source) = self.filesystem.remove(path).await {
                self.removals
                    .borrow_mut()
                    .extend(removals[index..].iter().cloned());
                return Err(PersistenceError::storage(
                    "remove state",
                    path.clone(),
                    source,
                ));
            }
            self.parts.borrow_mut().remove(address);
        }
        let parts = {
            let parts = self.parts.borrow();
            parts
                .iter()
                .map(|(address, part)| (address.clone(), Arc::clone(part)))
                .collect::<Vec<_>>()
        };
        let mut dropped = Vec::new();

        for (address, part) in parts {
            let path = self.state_path(&address);
            match part
                .snapshot_if_dirty()
                .map_err(|source| PersistenceError::codec(path.clone(), source))?
            {
                PartStatus::Dropped => dropped.push(address),
                PartStatus::Clean => {}
                PartStatus::Dirty(snapshot) => {
                    let file = encode_file(snapshot.schema_version, snapshot.state);
                    self.filesystem
                        .write_atomic(&path, &file)
                        .await
                        .map_err(|source| PersistenceError::storage("write state", path, source))?;
                    part.mark_persisted(snapshot.generation);
                }
            }
        }

        if !dropped.is_empty() {
            let mut parts = self.parts.borrow_mut();
            for address in dropped {
                let should_remove = parts.get(&address).is_some_and(|part| !part.is_alive());
                if should_remove {
                    parts.remove(&address);
                }
            }
        }

        Ok(())
    }

    fn ensure_entry_type<T>(&self, entry: &EntryKey) -> Result<(), PersistenceError>
    where
        T: 'static,
    {
        self.validate_entry(entry)?;
        let entry_type = RegisteredEntryType {
            state_type_id: TypeId::of::<T>(),
            state_type_name: type_name::<T>(),
        };
        let mut entry_types = self.entry_types.borrow_mut();
        match entry_types.entry(entry.clone()) {
            MapEntry::Vacant(slot) => {
                slot.insert(entry_type);
                Ok(())
            }
            MapEntry::Occupied(slot) if slot.get().state_type_id == entry_type.state_type_id => {
                Ok(())
            }
            MapEntry::Occupied(slot) => Err(PersistenceError::TypeMismatch {
                kind: entry.kind(),
                name: entry.name().to_owned(),
                expected: entry_type.state_type_name,
                actual: slot.get().state_type_name,
            }),
        }
    }

    async fn load_at<T>(&self, address: &StateAddress) -> Result<Option<T>, PersistenceError>
    where
        T: DurableStateCodec,
    {
        let path = self.state_path(address);
        let file = match self.filesystem.read(&path).await {
            Ok(file) => file,
            Err(FsError::NotFound) => return Ok(None),
            Err(source) => return Err(PersistenceError::storage("read state", path, source)),
        };
        let (schema_version, state) = decode_file(&path, &file)?;
        T::decode_state(schema_version, state)
            .map(Some)
            .map_err(|source| PersistenceError::codec(path, source))
    }

    fn register_state<T>(
        &self,
        address: StateAddress,
        name: String,
        instance_id: Option<InstanceId>,
        state: &DurableState<T>,
    ) -> Result<(), PersistenceError>
    where
        T: DurableStateCodec + 'static,
    {
        self.register_part(
            address,
            name,
            instance_id,
            Arc::new(StateRegisteredPart {
                state: state.downgrade(),
                persisted_generation: RefCell::new(None),
            }),
        )
    }

    fn register_part(
        &self,
        address: StateAddress,
        name: String,
        instance_id: Option<InstanceId>,
        part: Arc<dyn RegisteredPart>,
    ) -> Result<(), PersistenceError> {
        let mut parts = self.parts.borrow_mut();
        match parts.entry(address) {
            MapEntry::Vacant(slot) => {
                slot.insert(part);
                Ok(())
            }
            MapEntry::Occupied(mut slot) if !slot.get().is_alive() => {
                slot.insert(part);
                Ok(())
            }
            MapEntry::Occupied(_) => {
                Err(PersistenceError::StateAlreadyRegistered { name, instance_id })
            }
        }
    }

    fn remove_at(&self, address: &StateAddress) -> Result<(), PersistenceError> {
        let path = self.state_path(address);
        self.parts.borrow_mut().remove(address);
        self.removals.borrow_mut().push((address.clone(), path));
        Ok(())
    }

    async fn list_collection(&self, name: &str) -> Result<Vec<InstanceId>, PersistenceError> {
        let path = self.join_path(name);
        let entries = match self.filesystem.list_dir(&path).await {
            Ok(entries) => entries,
            Err(FsError::NotFound) => return Ok(Vec::new()),
            Err(source) => {
                return Err(PersistenceError::storage("list collection", path, source));
            }
        };

        let mut instance_ids = entries
            .into_iter()
            .filter_map(|entry| {
                entry
                    .strip_suffix(FILE_EXTENSION)
                    .and_then(|instance_id| InstanceId::new(instance_id).ok())
            })
            .collect::<Vec<_>>();
        instance_ids.sort_unstable();
        Ok(instance_ids)
    }

    fn validate_entry(&self, entry: &EntryKey) -> Result<(), PersistenceError> {
        if is_valid_key(entry.name()) {
            return Ok(());
        }
        match entry {
            EntryKey::Singleton(name) => {
                Err(PersistenceError::InvalidSingleton { name: name.clone() })
            }
            EntryKey::Collection(name) => {
                Err(PersistenceError::InvalidCollection { name: name.clone() })
            }
        }
    }

    fn state_path(&self, address: &StateAddress) -> String {
        let relative = match address {
            StateAddress::Singleton { name } => format!("{name}{FILE_EXTENSION}"),
            StateAddress::Collection { name, instance_id } => {
                format!("{name}/{}{FILE_EXTENSION}", instance_id.as_str())
            }
        };
        self.join_path(&relative)
    }

    fn join_path(&self, relative: &str) -> String {
        if self.persistence_directory == "/" {
            format!("/{relative}")
        } else if self.persistence_directory.ends_with('/') {
            format!("{}{relative}", self.persistence_directory)
        } else {
            format!("{}/{relative}", self.persistence_directory)
        }
    }
}

impl<T: DurableStateCodec + 'static> Singleton<'_, T> {
    /// Decode the persisted DTO, returning `None` when no state exists.
    pub async fn load(&self) -> Result<Option<T>, PersistenceError> {
        self.persistence
            .load_at(&StateAddress::Singleton {
                name: self.name.clone(),
            })
            .await
    }

    /// Register the runtime owner's state for automatic persistence.
    pub fn register(&self, state: &DurableState<T>) -> Result<(), PersistenceError> {
        self.persistence.register_state(
            StateAddress::Singleton {
                name: self.name.clone(),
            },
            self.name.clone(),
            None,
            state,
        )
    }

    /// Remove the persisted singleton and its live registration.
    pub fn remove(&self) -> Result<(), PersistenceError> {
        self.persistence.remove_at(&StateAddress::Singleton {
            name: self.name.clone(),
        })
    }
}

impl<T: DurableStateCodec + 'static> Collection<'_, T> {
    /// List the persisted instance identifiers.
    pub async fn list(&self) -> Result<Vec<InstanceId>, PersistenceError> {
        self.persistence.list_collection(&self.name).await
    }

    /// Decode one persisted DTO, returning `None` when it does not exist.
    pub async fn load(&self, instance_id: &InstanceId) -> Result<Option<T>, PersistenceError> {
        self.persistence
            .load_at(&StateAddress::Collection {
                name: self.name.clone(),
                instance_id: instance_id.clone(),
            })
            .await
    }

    /// Register one runtime-owned collection state for automatic persistence.
    pub fn register(
        &self,
        instance_id: &InstanceId,
        state: &DurableState<T>,
    ) -> Result<(), PersistenceError> {
        self.persistence.register_state(
            StateAddress::Collection {
                name: self.name.clone(),
                instance_id: instance_id.clone(),
            },
            self.name.clone(),
            Some(instance_id.clone()),
            state,
        )
    }

    /// Remove one persisted instance and its live registration.
    pub fn remove(&self, instance_id: &InstanceId) -> Result<(), PersistenceError> {
        self.persistence.remove_at(&StateAddress::Collection {
            name: self.name.clone(),
            instance_id: instance_id.clone(),
        })
    }
}

#[derive(Clone, Copy)]
struct RegisteredEntryType {
    state_type_id: TypeId,
    state_type_name: &'static str,
}

trait RegisteredPart {
    fn is_alive(&self) -> bool;

    fn snapshot_if_dirty(&self) -> Result<PartStatus, DurablePartError>;

    fn mark_persisted(&self, generation: PartGeneration);
}

struct StateRegisteredPart<T> {
    state: WeakDurableState<T>,
    persisted_generation: RefCell<Option<PartGeneration>>,
}

impl<T> RegisteredPart for StateRegisteredPart<T>
where
    T: DurableStateCodec + 'static,
{
    fn is_alive(&self) -> bool {
        self.state.generation().is_some()
    }

    fn snapshot_if_dirty(&self) -> Result<PartStatus, DurablePartError> {
        let persisted_generation = *self.persisted_generation.borrow();
        let Some(generation) = self.state.generation() else {
            return Ok(PartStatus::Dropped);
        };
        if persisted_generation == Some(generation) {
            return Ok(PartStatus::Clean);
        }

        let Some(snapshot) = self.state.snapshot()? else {
            return Ok(PartStatus::Dropped);
        };
        let (generation, schema_version, state) = snapshot.into_parts();
        if persisted_generation == Some(generation) {
            return Ok(PartStatus::Clean);
        }
        Ok(PartStatus::Dirty(PartSnapshot {
            generation,
            schema_version,
            state,
        }))
    }

    fn mark_persisted(&self, generation: PartGeneration) {
        *self.persisted_generation.borrow_mut() = Some(generation);
    }
}

enum PartStatus {
    Dropped,
    Clean,
    Dirty(PartSnapshot),
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
        return Err(PersistenceError::corrupt_state(
            path.to_owned(),
            SCHEMA_VERSION_SIZE,
            file.len(),
        ));
    }

    let schema_version = SchemaVersion::from_le_bytes([file[0], file[1], file[2], file[3]]);
    Ok((
        schema_version,
        StateSlice {
            bytes: &file[SCHEMA_VERSION_SIZE..],
        },
    ))
}

/// Errors produced by the persistence registry.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("persistence directory cannot be empty")]
    EmptyDirectory,
    #[error("invalid singleton name `{name}`")]
    InvalidSingleton { name: String },
    #[error("invalid collection name `{name}`")]
    InvalidCollection { name: String },
    #[error(
        "persistence type mismatch for {kind} `{name}`: requested {expected}, registered {actual}"
    )]
    TypeMismatch {
        kind: &'static str,
        name: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("persistence state `{name}` is already registered with instance id {instance_id:?}")]
    StateAlreadyRegistered {
        name: String,
        instance_id: Option<InstanceId>,
    },
    #[error("{0}")]
    Storage(#[source] Box<dyn Error + Send + Sync + 'static>),
    #[error("{0}")]
    CorruptState(#[source] Box<dyn Error + Send + Sync + 'static>),
    #[error("{0}")]
    Codec(#[source] Box<dyn Error + Send + Sync + 'static>),
}

impl PersistenceError {
    fn storage(operation: &'static str, path: String, source: FsError) -> Self {
        Self::Storage(Box::new(StorageFailure {
            operation,
            path,
            source,
        }))
    }

    fn corrupt_state(path: String, expected_size: usize, actual_size: usize) -> Self {
        Self::CorruptState(Box::new(CorruptStateFailure {
            path,
            expected_size,
            actual_size,
        }))
    }

    fn codec(path: String, source: DurablePartError) -> Self {
        Self::Codec(Box::new(CodecFailure { path, source }))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("persistence storage operation failed to {operation} at `{path}`: {source}")]
struct StorageFailure {
    operation: &'static str,
    path: String,
    #[source]
    source: FsError,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "persisted state `{path}` is too short: expected at least {expected_size} bytes, found {actual_size}"
)]
struct CorruptStateFailure {
    path: String,
    expected_size: usize,
    actual_size: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("failed to process persisted state `{path}`: {source}")]
struct CodecFailure {
    path: String,
    #[source]
    source: DurablePartError,
}

#[cfg(test)]
mod tests {
    use alloc::borrow::Cow;
    use alloc::string::ToString;
    use alloc::vec;

    use barracuda_platform_test::memory_vfs;
    use barracuda_vfs::ScopedVfs;
    use futures_lite::future::block_on;

    use super::*;

    #[derive(Debug)]
    struct TestState {
        value: u32,
    }

    impl DurableStateCodec for TestState {
        const SCHEMA_VERSION: SchemaVersion = 1;

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
            if state.bytes.len() != core::mem::size_of::<u32>() {
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

    fn instance_id(id: &str) -> InstanceId {
        InstanceId::new(id).expect("test instance id is valid")
    }

    async fn fixture(root: &str) -> (ScopedVfs, Persistence) {
        let filesystem = memory_vfs().await.expect("memory VFS mounts");
        let persistence = Persistence::new(filesystem.clone(), root)
            .await
            .expect("persistence initializes");
        (filesystem, persistence)
    }

    #[test]
    fn singleton_registers_persists_and_loads() {
        block_on(async {
            let root = "/barracuda-agent-persistence-singleton";
            let (filesystem, persistence) = fixture(root).await;
            let singleton = persistence
                .singleton::<TestState>("state")
                .expect("singleton opens");
            assert!(singleton
                .load()
                .await
                .expect("missing state is readable")
                .is_none());

            let state = DurableState::new(TestState { value: 1 });
            singleton.register(&state).expect("state registers");
            state.get_mut().value = 2;
            persistence
                .maybe_persist()
                .await
                .expect("dirty state persists");

            let file = filesystem
                .read(&format!("{root}/state.bin"))
                .await
                .expect("state file exists");
            assert_eq!(&file[..SCHEMA_VERSION_SIZE], &1_u32.to_le_bytes());
            assert_eq!(&file[SCHEMA_VERSION_SIZE..], &2_u32.to_le_bytes());

            let restored = Persistence::new(filesystem.clone(), root)
                .await
                .expect("persistence reinitializes");
            let singleton = restored
                .singleton::<TestState>("state")
                .expect("singleton reopens");
            assert_eq!(singleton.load().await.unwrap().unwrap().value, 2);
        });
    }

    #[test]
    fn collection_serves_multiple_instances() {
        block_on(async {
            let root = "/barracuda-agent-persistence-collection";
            let (_filesystem, persistence) = fixture(root).await;
            let collection = persistence
                .collection::<TestState>("sessions")
                .expect("collection opens");
            let session_2 = instance_id("session-2");
            let session_10 = instance_id("session-10");
            let state_2 = DurableState::new(TestState { value: 2 });
            let state_10 = DurableState::new(TestState { value: 10 });
            collection.register(&session_2, &state_2).unwrap();
            collection.register(&session_10, &state_10).unwrap();

            assert!(collection.list().await.unwrap().is_empty());
            persistence.maybe_persist().await.unwrap();
            assert_eq!(
                collection.list().await.unwrap(),
                vec![session_10.clone(), session_2]
            );
            assert_eq!(
                collection.load(&session_10).await.unwrap().unwrap().value,
                10
            );
        });
    }

    #[test]
    fn registration_is_non_owning() {
        block_on(async {
            let root = "/barracuda-agent-persistence-weak-registration";
            let (filesystem, persistence) = fixture(root).await;
            let singleton = persistence.singleton::<TestState>("state").unwrap();
            let state = DurableState::new(TestState { value: 1 });
            singleton.register(&state).unwrap();
            drop(state);

            persistence.maybe_persist().await.unwrap();
            assert!(!filesystem
                .exists(&format!("{root}/state.bin"))
                .await
                .expect("existence check succeeds"));
        });
    }

    #[test]
    fn a_dropped_owner_can_be_replaced_before_cleanup() {
        block_on(async {
            let (_filesystem, persistence) =
                fixture("/barracuda-agent-persistence-reregister").await;
            let singleton = persistence.singleton::<TestState>("state").unwrap();
            let first = DurableState::new(TestState { value: 1 });
            singleton.register(&first).unwrap();
            drop(first);

            let second = DurableState::new(TestState { value: 2 });
            singleton.register(&second).unwrap();
            persistence.maybe_persist().await.unwrap();
            assert_eq!(singleton.load().await.unwrap().unwrap().value, 2);
        });
    }

    #[test]
    fn duplicate_live_registration_is_rejected() {
        block_on(async {
            let (_filesystem, persistence) =
                fixture("/barracuda-agent-persistence-duplicate").await;
            let singleton = persistence.singleton::<TestState>("state").unwrap();
            let first = DurableState::new(TestState { value: 1 });
            let second = DurableState::new(TestState { value: 2 });
            singleton.register(&first).unwrap();

            assert!(matches!(
                singleton.register(&second),
                Err(PersistenceError::StateAlreadyRegistered { .. })
            ));
        });
    }

    #[test]
    fn entry_type_is_stable() {
        block_on(async {
            let (_filesystem, persistence) = fixture("/barracuda-agent-persistence-type").await;
            persistence.singleton::<TestState>("state").unwrap();
            persistence.singleton::<TestState>("state").unwrap();
            assert!(matches!(
                persistence.singleton::<OtherState>("state"),
                Err(PersistenceError::TypeMismatch { .. })
            ));
        });
    }

    #[test]
    fn singleton_and_collection_names_do_not_collide() {
        block_on(async {
            let root = "/barracuda-agent-persistence-peer-entries";
            let (filesystem, persistence) = fixture(root).await;
            let singleton = persistence.singleton::<TestState>("sessions").unwrap();
            let collection = persistence.collection::<OtherState>("sessions").unwrap();
            let singleton_state = DurableState::new(TestState { value: 1 });
            let collection_state = DurableState::new(OtherState);
            singleton.register(&singleton_state).unwrap();
            collection
                .register(&instance_id("session-1"), &collection_state)
                .unwrap();
            persistence.maybe_persist().await.unwrap();

            assert!(filesystem
                .exists(&format!("{root}/sessions.bin"))
                .await
                .expect("existence check succeeds"));
            assert!(filesystem
                .exists(&format!("{root}/sessions/session-1.bin"))
                .await
                .expect("existence check succeeds"));
        });
    }

    #[test]
    fn entry_names_are_identifiers_not_paths() {
        block_on(async {
            let (_filesystem, persistence) = fixture("/barracuda-agent-persistence-names").await;
            assert!(matches!(
                persistence.singleton::<TestState>("nested/state"),
                Err(PersistenceError::InvalidSingleton { .. })
            ));
            assert!(matches!(
                persistence.collection::<TestState>("nested/sessions"),
                Err(PersistenceError::InvalidCollection { .. })
            ));
        });
    }

    #[test]
    fn remove_deletes_state_and_registration() {
        block_on(async {
            let root = "/barracuda-agent-persistence-remove";
            let (filesystem, persistence) = fixture(root).await;
            let collection = persistence.collection::<TestState>("sessions").unwrap();
            let id = instance_id("session-1");
            let state = DurableState::new(TestState { value: 1 });
            collection.register(&id, &state).unwrap();
            persistence.maybe_persist().await.unwrap();
            collection.remove(&id).unwrap();
            state.get_mut().value = 2;
            persistence.maybe_persist().await.unwrap();

            assert!(collection.load(&id).await.unwrap().is_none());
            assert!(!filesystem
                .exists(&format!("{root}/sessions/session-1.bin"))
                .await
                .expect("existence check succeeds"));
        });
    }

    #[test]
    fn list_filters_non_state_entries() {
        block_on(async {
            let root = "/barracuda-agent-persistence-list";
            let (filesystem, persistence) = fixture(root).await;
            let collection = persistence.collection::<TestState>("sessions").unwrap();
            filesystem
                .write_atomic(&format!("{root}/sessions/session-1.bin"), b"state")
                .await
                .unwrap();
            filesystem
                .write_atomic(&format!("{root}/sessions/transcript.jsonl"), b"ignored")
                .await
                .unwrap();
            filesystem
                .write_atomic(&format!("{root}/sessions/...bin"), b"ignored")
                .await
                .unwrap();

            assert_eq!(
                collection.list().await.unwrap(),
                vec![instance_id("session-1")]
            );
        });
    }

    #[test]
    fn load_rejects_a_truncated_schema_version() {
        block_on(async {
            let root = "/barracuda-agent-persistence-truncated";
            let path = format!("{root}/state.bin");
            let (filesystem, persistence) = fixture(root).await;
            let singleton = persistence.singleton::<TestState>("state").unwrap();
            filesystem.write_atomic(&path, &[1, 0, 0]).await.unwrap();

            let error = singleton
                .load()
                .await
                .expect_err("truncated state is rejected");
            assert!(matches!(&error, PersistenceError::CorruptState(_)));
            assert!(error.to_string().contains(&path));
        });
    }
}
