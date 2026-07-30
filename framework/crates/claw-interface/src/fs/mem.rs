use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{ClawFile, ClawFs, FsError};

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// In-memory [`ClawFs`] backed by a shared path → bytes map.
///
/// Each [`MemFs::new`] creates an independent namespace. The filesystem itself
/// is deliberately not [`Clone`]; owners that intentionally share one instance
/// do so explicitly through an [`Arc`]. Dropping the last filesystem or open
/// file handle releases every file.
/// `list_dir` derives entries from the key prefixes, mirroring a real
/// directory tree.
#[derive(Debug)]
pub struct MemFs {
    files: Files,
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemFs {
    /// An empty filesystem.
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Remove every file from this namespace.
    pub fn clear(&self) {
        Self::lock(&self.files).clear();
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

    fn open(&self, path: &str) -> Result<Self::File, FsError> {
        let files = Arc::clone(&self.files);
        if Self::lock(&files).contains_key(path) {
            Ok(MemFile {
                files,
                path: path.to_string(),
            })
        } else {
            Err(FsError::NotFound)
        }
    }

    fn create(&self, path: &str) -> Result<Self::File, FsError> {
        let files = Arc::clone(&self.files);
        // Truncate: an empty entry that subsequent `write_all`s extend.
        Self::lock(&files).insert(path.to_string(), Vec::new());
        Ok(MemFile {
            files,
            path: path.to_string(),
        })
    }

    fn open_append(&self, path: &str) -> Result<Self::File, FsError> {
        let files = Arc::clone(&self.files);
        Self::lock(&files).entry(path.to_string()).or_default();
        Ok(MemFile {
            files,
            path: path.to_string(),
        })
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let files = Arc::clone(&self.files);
        let mut files = Self::lock(&files);
        let bytes = files.remove(from).ok_or(FsError::NotFound)?;
        files.insert(to.to_string(), bytes);
        Ok(())
    }

    fn create_dir_all(&self, _path: &str) -> Result<(), FsError> {
        // Flat key→bytes map: directories are implicit in key prefixes, so
        // there is nothing to materialize. `list_dir` of an empty prefix
        // already returns an empty listing.
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        let files = Arc::clone(&self.files);
        let exists = Self::lock(&files).contains_key(path);
        exists
    }

    fn remove(&self, path: &str) -> Result<(), FsError> {
        let files = Arc::clone(&self.files);
        Self::lock(&files).remove(path);
        Ok(())
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut names = BTreeSet::new();
        let files = Arc::clone(&self.files);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MemFs>();
    }

    #[test]
    fn new_instances_have_independent_namespaces() {
        let first = MemFs::new();
        let second = MemFs::new();

        first.write_atomic("/state", b"first").unwrap();

        assert_eq!(first.read("/state").unwrap(), b"first");
        assert_eq!(second.read("/state"), Err(FsError::NotFound));
    }
}
