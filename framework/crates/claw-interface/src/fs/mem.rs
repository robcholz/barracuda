use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{ClawFile, ClawFs, FsError};

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

thread_local! {
    static FILES: RefCell<Files> = RefCell::new(Arc::new(Mutex::new(HashMap::new())));
}

/// In-memory [`ClawFs`] backed by a thread-local path → bytes map.
///
/// `MemFs` is a backend type, not a storage handle. [`MemFs::new`] resets the
/// current thread's test fixture and returns the zero-sized selector. File
/// handles keep an `Arc` to the fixture they were opened against, so already
/// opened handles remain valid even if a later test reset installs a fresh
/// fixture.
///
/// Hermetic per test thread, so host tests can exercise persistence without
/// touching the real filesystem.
/// `list_dir` derives entries from the key prefixes, mirroring a real
/// directory tree.
///
/// [`DiskFs`]: super::DiskFs
#[derive(Debug, Clone, Copy)]
pub struct MemFs;

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemFs {
    /// An empty filesystem.
    pub fn new() -> Self {
        FILES.with(|slot| *slot.borrow_mut() = Arc::new(Mutex::new(HashMap::new())));
        Self
    }

    /// Clear the current thread's in-memory filesystem fixture.
    pub fn clear() {
        FILES.with(|slot| {
            slot.borrow()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        });
    }

    fn files() -> Files {
        FILES.with(|slot| Arc::clone(&slot.borrow()))
    }

    fn lock(files: &Files) -> MutexGuard<'_, HashMap<String, Vec<u8>>> {
        files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// An open handle into a [`MemFs`] store.
///
/// Holds the shared store plus the key it addresses; reads slice the live
/// bytes under the lock, writes extend them. Writes go to the store
/// immediately (there is no separate "flush"), matching the on-disk backend
/// closely enough for tests.
pub struct MemFile {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    path: String,
}

impl MemFile {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<u8>>> {
        MemFs::lock(&self.files)
    }
}

impl ClawFile for MemFile {
    fn read_to_end(&mut self) -> Result<Vec<u8>, FsError> {
        self.lock()
            .get(&self.path)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    fn read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let files = self.lock();
        let bytes = files.get(&self.path).ok_or(FsError::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| FsError::io_message("offset overflow"))?;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| FsError::io_message("read_at past end of file"))?;
        bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| FsError::io_message("read_at past end of file"))
    }

    fn size(&self) -> Result<u64, FsError> {
        self.lock()
            .get(&self.path)
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or(FsError::NotFound)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), FsError> {
        self.lock()
            .entry(self.path.clone())
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }
}

impl ClawFs for MemFs {
    type File = MemFile;

    fn open(path: &str) -> Result<Self::File, FsError> {
        let files = Self::files();
        if Self::lock(&files).contains_key(path) {
            Ok(MemFile {
                files,
                path: path.to_string(),
            })
        } else {
            Err(FsError::NotFound)
        }
    }

    fn create(path: &str) -> Result<Self::File, FsError> {
        let files = Self::files();
        // Truncate: an empty entry that subsequent `write_all`s extend.
        Self::lock(&files).insert(path.to_string(), Vec::new());
        Ok(MemFile {
            files,
            path: path.to_string(),
        })
    }

    fn open_append(path: &str) -> Result<Self::File, FsError> {
        let files = Self::files();
        Self::lock(&files).entry(path.to_string()).or_default();
        Ok(MemFile {
            files,
            path: path.to_string(),
        })
    }

    fn rename(from: &str, to: &str) -> Result<(), FsError> {
        let files = Self::files();
        let mut files = Self::lock(&files);
        let bytes = files.remove(from).ok_or(FsError::NotFound)?;
        files.insert(to.to_string(), bytes);
        Ok(())
    }

    fn create_dir_all(_path: &str) -> Result<(), FsError> {
        // Flat key→bytes map: directories are implicit in key prefixes, so
        // there is nothing to materialize. `list_dir` of an empty prefix
        // already returns an empty listing.
        Ok(())
    }

    fn exists(path: &str) -> bool {
        let files = Self::files();
        let exists = Self::lock(&files).contains_key(path);
        exists
    }

    fn remove(path: &str) -> Result<(), FsError> {
        let files = Self::files();
        Self::lock(&files).remove(path);
        Ok(())
    }

    fn list_dir(path: &str) -> Result<Vec<String>, FsError> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut names = BTreeSet::new();
        let files = Self::files();
        for key in Self::lock(&files).keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                if let Some(name) = rest.split('/').next().filter(|name| !name.is_empty()) {
                    names.insert(name.to_string());
                }
            }
        }
        Ok(names.into_iter().collect())
    }
}
