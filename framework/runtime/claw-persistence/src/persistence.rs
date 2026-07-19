use std::{
    any::{type_name, Any, TypeId},
    collections::{hash_map::Entry as MapEntry, HashMap},
    marker::PhantomData,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use claw_interface::{ClawFs, FsError};

use crate::{
    is_valid_key, DurablePart, DurablePartError, DurableState, DurableStateCodec, Entry,
    InstanceId, PartGeneration, SchemaVersion, StateBlob, StateSlice,
};

const SCHEMA_VERSION_SIZE: usize = std::mem::size_of::<SchemaVersion>();
const FILE_EXTENSION: &str = ".bin";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StateAddress {
    Singleton {
        key: String,
    },
    Collection {
        namespace: String,
        instance_id: InstanceId,
    },
}

pub struct Persistence<Filesystem: ClawFs> {
    persistence_directory: String,
    templates: Mutex<HashMap<Entry, RegisteredTemplate>>,
    parts: Mutex<HashMap<StateAddress, Arc<dyn RegisteredPart>>>,
    operation_lock: Mutex<()>,
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
            templates: Mutex::new(HashMap::new()),
            parts: Mutex::new(HashMap::new()),
            operation_lock: Mutex::new(()),
            filesystem: PhantomData,
        })
    }

    pub fn create_template<T>(&self, entry: Entry) -> Result<(), PersistenceError>
    where
        T: DurableStateCodec + Send + 'static,
    {
        self.validate_entry(&entry)?;

        let template = RegisteredTemplate {
            state_type_id: TypeId::of::<T>(),
            state_type_name: type_name::<T>(),
            restore: restore_registered_part::<T>,
        };
        let mut templates = lock(&self.templates);
        match templates.entry(entry) {
            MapEntry::Vacant(slot) => {
                slot.insert(template);
                Ok(())
            }
            MapEntry::Occupied(slot) => Err(PersistenceError::TemplateAlreadyExists {
                entry: slot.key().clone(),
            }),
        }
    }

    pub fn put<T>(
        &self,
        entry: &Entry,
        instance_id: Option<InstanceId>,
        value: T,
    ) -> Result<DurableState<T>, PersistenceError>
    where
        T: DurableStateCodec + Send + 'static,
    {
        let address = self.resolve(entry, instance_id.as_ref())?;
        self.template_for::<T>(entry)?;
        let _operation = lock(&self.operation_lock);
        let mut parts = lock(&self.parts);

        if let Some(part) = parts.get(&address) {
            let state = self.typed_state::<T>(entry, part.as_ref())?;
            state.replace(value);
            return Ok(state);
        }

        let part = DurablePart::new(value);
        let state = part.state.clone();
        parts.insert(
            address,
            Arc::new(TypedRegisteredPart {
                part,
                persisted_generation: Mutex::new(None),
            }),
        );
        Ok(state)
    }

    pub fn get<T>(
        &self,
        entry: &Entry,
        instance_id: Option<&InstanceId>,
    ) -> Result<DurableState<T>, PersistenceError>
    where
        T: DurableStateCodec + Send + 'static,
    {
        let address = self.resolve(entry, instance_id)?;
        let template = self.template_for::<T>(entry)?;
        let _operation = lock(&self.operation_lock);

        {
            let parts = lock(&self.parts);
            if let Some(part) = parts.get(&address) {
                return self.typed_state::<T>(entry, part.as_ref());
            }
        }

        let path = self.state_path(&address);
        let file = match Filesystem::read(&path) {
            Ok(file) => file,
            Err(FsError::NotFound) => {
                return Err(PersistenceError::StateNotFound {
                    entry: entry.clone(),
                    instance_id: instance_id.cloned(),
                });
            }
            Err(source) => return Err(PersistenceError::Read { path, source }),
        };
        let (schema_version, state) = decode_file(&path, &file)?;
        let registered =
            (template.restore)(schema_version, state).map_err(|source| PersistenceError::Part {
                path: path.clone(),
                source,
            })?;
        let durable_state = self.typed_state::<T>(entry, registered.as_ref())?;
        lock(&self.parts).insert(address, registered);
        Ok(durable_state)
    }

    pub fn remove(
        &self,
        entry: &Entry,
        instance_id: Option<&InstanceId>,
    ) -> Result<(), PersistenceError> {
        let address = self.resolve(entry, instance_id)?;
        self.ensure_template(entry)?;
        let _operation = lock(&self.operation_lock);
        let path = self.state_path(&address);

        Filesystem::remove(&path).map_err(|source| PersistenceError::Remove { path, source })?;
        lock(&self.parts).remove(&address);
        Ok(())
    }

    pub fn list(&self, entry: &Entry) -> Result<Vec<InstanceId>, PersistenceError> {
        self.validate_entry(entry)?;
        self.ensure_template(entry)?;
        let Entry::Collection(namespace) = entry else {
            return Err(PersistenceError::CannotListSingleton {
                entry: entry.clone(),
            });
        };

        let _operation = lock(&self.operation_lock);
        let path = self.join_path(namespace);
        let entries = match Filesystem::list_dir(&path) {
            Ok(entries) => entries,
            Err(FsError::NotFound) => return Ok(Vec::new()),
            Err(source) => return Err(PersistenceError::List { path, source }),
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

    pub fn maybe_persist(&self) -> Result<(), PersistenceError> {
        let _operation = lock(&self.operation_lock);
        let parts = {
            let parts = lock(&self.parts);
            parts
                .iter()
                .map(|(address, part)| (address.clone(), Arc::clone(part)))
                .collect::<Vec<_>>()
        };

        for (address, part) in parts {
            let path = self.state_path(&address);
            let Some(snapshot) =
                part.snapshot_if_dirty()
                    .map_err(|source| PersistenceError::Part {
                        path: path.clone(),
                        source,
                    })?
            else {
                continue;
            };

            let file = encode_file(snapshot.schema_version, snapshot.state);
            Filesystem::write_atomic(&path, &file)
                .map_err(|source| PersistenceError::Write { path, source })?;
            part.mark_persisted(snapshot.generation);
        }

        Ok(())
    }

    fn template_for<T>(&self, entry: &Entry) -> Result<RegisteredTemplate, PersistenceError>
    where
        T: 'static,
    {
        let template = self.ensure_template(entry)?;
        if template.state_type_id != TypeId::of::<T>() {
            return Err(PersistenceError::TypeMismatch {
                entry: entry.clone(),
                expected: type_name::<T>(),
                actual: template.state_type_name,
            });
        }
        Ok(template)
    }

    fn ensure_template(&self, entry: &Entry) -> Result<RegisteredTemplate, PersistenceError> {
        lock(&self.templates).get(entry).copied().ok_or_else(|| {
            PersistenceError::TemplateNotFound {
                entry: entry.clone(),
            }
        })
    }

    fn typed_state<T>(
        &self,
        entry: &Entry,
        part: &dyn RegisteredPart,
    ) -> Result<DurableState<T>, PersistenceError>
    where
        T: 'static,
    {
        part.state()
            .downcast_ref::<DurableState<T>>()
            .cloned()
            .ok_or_else(|| PersistenceError::TypeMismatch {
                entry: entry.clone(),
                expected: type_name::<T>(),
                actual: part.state_type_name(),
            })
    }

    fn resolve(
        &self,
        entry: &Entry,
        instance_id: Option<&InstanceId>,
    ) -> Result<StateAddress, PersistenceError> {
        self.validate_entry(entry)?;

        match (entry, instance_id) {
            (Entry::Singleton(key), None) => Ok(StateAddress::Singleton { key: key.clone() }),
            (Entry::Singleton(_), Some(instance_id)) => {
                Err(PersistenceError::UnexpectedInstanceId {
                    entry: entry.clone(),
                    instance_id: instance_id.clone(),
                })
            }
            (Entry::Collection(namespace), Some(instance_id)) => Ok(StateAddress::Collection {
                namespace: namespace.clone(),
                instance_id: instance_id.clone(),
            }),
            (Entry::Collection(_), None) => Err(PersistenceError::MissingInstanceId {
                entry: entry.clone(),
            }),
        }
    }

    fn validate_entry(&self, entry: &Entry) -> Result<(), PersistenceError> {
        match entry {
            Entry::Singleton(key) => {
                if !is_valid_key(key) {
                    return Err(PersistenceError::InvalidSingleton { key: key.clone() });
                }
            }
            Entry::Collection(namespace) => {
                if !is_valid_namespace(namespace) {
                    return Err(PersistenceError::InvalidCollection {
                        namespace: namespace.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn state_path(&self, address: &StateAddress) -> String {
        let relative = match address {
            StateAddress::Singleton { key } => format!("{key}{FILE_EXTENSION}"),
            StateAddress::Collection {
                namespace,
                instance_id,
            } => format!("{namespace}/{}{FILE_EXTENSION}", instance_id.as_str()),
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

#[derive(Clone, Copy)]
struct RegisteredTemplate {
    state_type_id: TypeId,
    state_type_name: &'static str,
    restore: RestorePart,
}

type RestorePart =
    for<'a> fn(SchemaVersion, StateSlice<'a>) -> Result<Arc<dyn RegisteredPart>, DurablePartError>;

fn restore_registered_part<T>(
    schema_version: SchemaVersion,
    state: StateSlice<'_>,
) -> Result<Arc<dyn RegisteredPart>, DurablePartError>
where
    T: DurableStateCodec + Send + 'static,
{
    let part = DurablePart::<T>::restore(schema_version, state)?;
    let persisted_generation = part.generation();
    Ok(Arc::new(TypedRegisteredPart {
        part,
        persisted_generation: Mutex::new(Some(persisted_generation)),
    }))
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

fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && !namespace.starts_with('/')
        && namespace
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
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
    #[error("invalid singleton key `{key}`")]
    InvalidSingleton { key: String },
    #[error("invalid collection namespace `{namespace}`")]
    InvalidCollection { namespace: String },
    #[error("persistence template already exists for {entry:?}")]
    TemplateAlreadyExists { entry: Entry },
    #[error("persistence template does not exist for {entry:?}")]
    TemplateNotFound { entry: Entry },
    #[error("collection entry {entry:?} requires an instance id")]
    MissingInstanceId { entry: Entry },
    #[error("singleton entry {entry:?} does not accept instance id {instance_id:?}")]
    UnexpectedInstanceId {
        entry: Entry,
        instance_id: InstanceId,
    },
    #[error("persistence type mismatch for {entry:?}: requested {expected}, registered {actual}")]
    TypeMismatch {
        entry: Entry,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("persisted state does not exist for {entry:?} with instance id {instance_id:?}")]
    StateNotFound {
        entry: Entry,
        instance_id: Option<InstanceId>,
    },
    #[error("cannot list singleton entry {entry:?}")]
    CannotListSingleton { entry: Entry },
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
    #[error("failed to list persistence collection `{path}`: {source}")]
    List {
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
    #[error("failed to remove persistence file `{path}`: {source}")]
    Remove {
        path: String,
        #[source]
        source: FsError,
    },
    #[error(
        "persistence file `{path}` is too short: expected at least 4 bytes, found {actual_size}"
    )]
    TruncatedFile { path: String, actual_size: usize },
    #[error("failed to process persisted state `{path}`: {source}")]
    Part {
        path: String,
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

    fn instance_id(id: &str) -> InstanceId {
        InstanceId::new(id).expect("test instance id is valid")
    }

    #[test]
    fn singleton_put_get_persist_and_restore() {
        let root = "/claw-persistence-singleton";
        let entry = Entry::singleton("state");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("singleton template is created");

        let state = persistence
            .put(&entry, None, TestState { value: 1 })
            .expect("singleton state is put");
        state.get_mut().value = 2;
        assert_eq!(
            persistence
                .get::<TestState>(&entry, None)
                .expect("singleton state is available")
                .get()
                .value,
            2
        );
        persistence.maybe_persist().expect("dirty state persists");

        let file = MemFs::read(&format!("{root}/state.bin")).expect("state file exists");
        assert_eq!(&file[..SCHEMA_VERSION_SIZE], &7_u32.to_le_bytes());
        assert_eq!(&file[SCHEMA_VERSION_SIZE..], &2_u32.to_le_bytes());

        let restored = Persistence::<MemFs>::new(root).expect("persistence reinitializes");
        restored
            .create_template::<TestState>(entry.clone())
            .expect("singleton template is recreated");
        assert_eq!(
            restored
                .get::<TestState>(&entry, None)
                .expect("singleton state restores")
                .get()
                .value,
            2
        );
    }

    #[test]
    fn collection_template_serves_multiple_instances() {
        let root = "/claw-persistence-collection";
        let entry = Entry::collection("sessions");
        let session_2 = instance_id("session-2");
        let session_10 = instance_id("session-10");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("collection template is created once");

        persistence
            .put(&entry, Some(session_2.clone()), TestState { value: 2 })
            .expect("first collection state is put");
        persistence
            .put(&entry, Some(session_10.clone()), TestState { value: 10 })
            .expect("second collection state is put");

        assert!(persistence
            .list(&entry)
            .expect("collection lists before persistence")
            .is_empty());
        persistence
            .maybe_persist()
            .expect("collection states persist");
        assert_eq!(
            persistence.list(&entry).expect("collection lists"),
            vec![session_10.clone(), session_2.clone()]
        );
        assert!(MemFs::exists(&format!("{root}/sessions/session-2.bin")));

        let restored = Persistence::<MemFs>::new(root).expect("persistence reinitializes");
        restored
            .create_template::<TestState>(entry.clone())
            .expect("collection template is recreated");
        assert_eq!(
            restored
                .get::<TestState>(&entry, Some(&session_10))
                .expect("one collection state restores")
                .get()
                .value,
            10
        );
    }

    #[test]
    fn put_updates_the_existing_shared_state() {
        let entry = Entry::singleton("state");
        let persistence = Persistence::<MemFs>::new("/claw-persistence-put-update")
            .expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("template is created");
        let first = persistence
            .put(&entry, None, TestState { value: 1 })
            .expect("state is put");
        let second = persistence
            .put(&entry, None, TestState { value: 2 })
            .expect("state is updated");

        assert_eq!(first.get().value, 2);
        assert_eq!(second.get().value, 2);
    }

    #[test]
    fn instance_id_rules_follow_the_entry_kind() {
        let persistence = Persistence::<MemFs>::new("/claw-persistence-instance-rules")
            .expect("persistence initializes");
        let singleton = Entry::singleton("state");
        let collection = Entry::collection("sessions");
        persistence
            .create_template::<TestState>(singleton.clone())
            .expect("singleton template is created");
        persistence
            .create_template::<TestState>(collection.clone())
            .expect("collection template is created");

        assert!(matches!(
            persistence.put(
                &singleton,
                Some(instance_id("unexpected")),
                TestState { value: 1 }
            ),
            Err(PersistenceError::UnexpectedInstanceId { .. })
        ));
        assert!(matches!(
            persistence.put(&collection, None, TestState { value: 1 }),
            Err(PersistenceError::MissingInstanceId { .. })
        ));
    }

    #[test]
    fn template_registration_enforces_identity_and_type() {
        let entry = Entry::singleton("state");
        let persistence = Persistence::<MemFs>::new("/claw-persistence-template-rules")
            .expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("template is created");

        assert!(matches!(
            persistence.create_template::<TestState>(entry.clone()),
            Err(PersistenceError::TemplateAlreadyExists { .. })
        ));
        assert!(matches!(
            persistence.put(&entry, None, OtherState),
            Err(PersistenceError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn singleton_and_collection_with_the_same_name_are_distinct_templates() {
        let root = "/claw-persistence-peer-entries";
        let singleton = Entry::singleton("sessions");
        let collection = Entry::collection("sessions");
        let instance_id = instance_id("session-1");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(singleton.clone())
            .expect("singleton template is created");
        persistence
            .create_template::<OtherState>(collection.clone())
            .expect("collection template with the same name is created");

        persistence
            .put(&singleton, None, TestState { value: 1 })
            .expect("singleton state is put");
        persistence
            .put(&collection, Some(instance_id), OtherState)
            .expect("collection state is put");
        persistence.maybe_persist().expect("both states persist");

        assert!(MemFs::exists(&format!("{root}/sessions.bin")));
        assert!(MemFs::exists(&format!("{root}/sessions/session-1.bin")));
    }

    #[test]
    fn remove_deletes_state_but_keeps_the_template() {
        let root = "/claw-persistence-remove";
        let entry = Entry::collection("sessions");
        let instance_id = instance_id("session-1");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("template is created");
        let detached = persistence
            .put(&entry, Some(instance_id.clone()), TestState { value: 1 })
            .expect("state is put");
        persistence.maybe_persist().expect("state persists");

        persistence
            .remove(&entry, Some(&instance_id))
            .expect("state is removed");
        detached.get_mut().value = 2;
        persistence
            .maybe_persist()
            .expect("detached state is not persisted");
        assert!(!MemFs::exists(&format!("{root}/sessions/session-1.bin")));
        assert!(matches!(
            persistence.get::<TestState>(&entry, Some(&instance_id)),
            Err(PersistenceError::StateNotFound { .. })
        ));

        persistence
            .put(&entry, Some(instance_id), TestState { value: 3 })
            .expect("the existing template accepts a new state");
    }

    #[test]
    fn list_filters_non_state_entries_and_rejects_singletons() {
        let root = "/claw-persistence-list";
        let collection = Entry::collection("sessions");
        let singleton = Entry::singleton("state");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(collection.clone())
            .expect("collection template is created");
        persistence
            .create_template::<TestState>(singleton.clone())
            .expect("singleton template is created");
        MemFs::write_atomic(&format!("{root}/sessions/session-1.bin"), b"state")
            .expect("state-looking file is installed");
        MemFs::write_atomic(&format!("{root}/sessions/transcript.jsonl"), b"ignored")
            .expect("non-state file is installed");
        MemFs::write_atomic(&format!("{root}/sessions/nested/state.bin"), b"ignored")
            .expect("nested state is installed");
        MemFs::write_atomic(&format!("{root}/sessions/...bin"), b"ignored")
            .expect("invalid state-looking file is installed");

        assert_eq!(
            persistence.list(&collection).expect("collection lists"),
            vec![instance_id("session-1")]
        );
        assert!(matches!(
            persistence.list(&singleton),
            Err(PersistenceError::CannotListSingleton { .. })
        ));
    }

    #[test]
    fn get_rejects_a_truncated_schema_version() {
        let root = "/claw-persistence-truncated-version";
        let path = format!("{root}/state.bin");
        let entry = Entry::singleton("state");
        let persistence = Persistence::<MemFs>::new(root).expect("persistence initializes");
        persistence
            .create_template::<TestState>(entry.clone())
            .expect("template is created");
        MemFs::write_atomic(&path, &[1, 0, 0]).expect("truncated file is installed");

        assert!(matches!(
            persistence.get::<TestState>(&entry, None),
            Err(PersistenceError::TruncatedFile { actual_size: 3, .. })
        ));
    }
}
