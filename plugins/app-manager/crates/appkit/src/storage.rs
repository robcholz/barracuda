//! Each app's own data: a key-value store and a file tree under
//! `/media/data/<id>/` in this Plugin's private filesystem.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use barracuda_vfs::{FsError, ScopedVfs};
use serde_json::{Map, Value};

/// The directory holding every app's data, `<id>/` each.
pub const APP_DATA: &str = "/media/data";
/// The largest key-value store an app may keep, as stored JSON.
pub const MAX_KV_BYTES: usize = 16 * 1024;
/// The longest key-value key.
pub const MAX_KEY_BYTES: usize = 64;
/// The largest file an app may read or write.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_PATH_DEPTH: usize = 8;
const MAX_NAME_BYTES: usize = 64;

/// The failure Lua sees as the second result after `nil`.
pub(crate) type Failure = String;

#[derive(Clone)]
pub(crate) struct Storage {
    filesystem: ScopedVfs,
    root: String,
}

fn fs_failure(error: FsError) -> Failure {
    String::from(match error {
        FsError::NotFound => "no such file or directory",
        FsError::IsDirectory => "is a directory",
        FsError::NotDirectory => "not a directory",
        FsError::DirectoryNotEmpty => "directory not empty",
        FsError::AlreadyExists => "already exists",
        _ => "storage failed",
    })
}

impl Storage {
    pub(crate) fn new(filesystem: ScopedVfs, id: &str) -> Self {
        Self {
            filesystem,
            root: format!("{APP_DATA}/{id}"),
        }
    }

    fn kv_path(&self) -> String {
        format!("{}/kv.json", self.root)
    }

    async fn kv_load(&self) -> Result<Map<String, Value>, Failure> {
        let bytes = match self.filesystem.read(&self.kv_path()).await {
            Ok(bytes) => bytes,
            Err(FsError::NotFound) => return Ok(Map::new()),
            Err(error) => return Err(fs_failure(error)),
        };
        serde_json::from_slice(&bytes).map_err(|_| String::from("the key-value store is damaged"))
    }

    fn check_key(key: &str) -> Result<(), Failure> {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(format!("keys are 1-{MAX_KEY_BYTES} bytes"));
        }
        Ok(())
    }

    /// The stored JSON for `key`, if any.
    pub(crate) async fn kv_get(&self, key: &str) -> Result<Option<String>, Failure> {
        Self::check_key(key)?;
        Ok(self
            .kv_load()
            .await?
            .get(key)
            .map(|value| value.to_string()))
    }

    /// Stores JSON under `key`; `None` deletes it.
    pub(crate) async fn kv_set(&self, key: &str, json: Option<&str>) -> Result<(), Failure> {
        Self::check_key(key)?;
        let value = json
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| String::from("the value is not storable"))?;
        let mut map = self.kv_load().await?;
        match value {
            Some(value) => {
                map.insert(String::from(key), value);
            }
            None => {
                if map.remove(key).is_none() {
                    return Ok(());
                }
            }
        }
        let bytes =
            serde_json::to_vec(&map).map_err(|_| String::from("the value is not storable"))?;
        if bytes.len() > MAX_KV_BYTES {
            return Err(format!(
                "the key-value store would exceed {MAX_KV_BYTES} bytes"
            ));
        }
        self.filesystem
            .write_atomic(&self.kv_path(), &bytes)
            .await
            .map_err(fs_failure)
    }

    /// Every key, sorted, as a JSON array.
    pub(crate) async fn kv_keys(&self) -> Result<String, Failure> {
        let keys: Vec<String> = self
            .kv_load()
            .await?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        serde_json::to_string(&keys).map_err(|_| String::from("storage failed"))
    }

    /// Maps an app path to its place under `files/`; `/` and `` name the root.
    fn file(&self, path: &str) -> Result<String, Failure> {
        let mut resolved = format!("{}/files", self.root);
        let mut depth = 0;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            depth += 1;
            if part == "." || part == ".." || part.len() > MAX_NAME_BYTES || depth > MAX_PATH_DEPTH
            {
                return Err(format!(
                    "paths stay inside the app: at most {MAX_PATH_DEPTH} names of 1-{MAX_NAME_BYTES} bytes, no . or .."
                ));
            }
            resolved.push('/');
            resolved.push_str(part);
        }
        Ok(resolved)
    }

    fn file_name(&self, path: &str) -> Result<String, Failure> {
        let resolved = self.file(path)?;
        if resolved.len() == self.root.len() + "/files".len() {
            return Err(String::from("is a directory"));
        }
        Ok(resolved)
    }

    pub(crate) async fn read(&self, path: &str) -> Result<Vec<u8>, Failure> {
        let path = self.file_name(path)?;
        let len = self.filesystem.len(&path).await.map_err(fs_failure)?;
        if len > MAX_FILE_BYTES {
            return Err(format!("files are at most {MAX_FILE_BYTES} bytes"));
        }
        self.filesystem.read(&path).await.map_err(fs_failure)
    }

    pub(crate) async fn write(&self, path: &str, data: &[u8], append: bool) -> Result<(), Failure> {
        let path = self.file_name(path)?;
        let existing = if append {
            match self.filesystem.len(&path).await {
                Ok(len) => len,
                Err(FsError::NotFound) => 0,
                Err(error) => return Err(fs_failure(error)),
            }
        } else {
            0
        };
        if existing.saturating_add(data.len() as u64) > MAX_FILE_BYTES {
            return Err(format!("files are at most {MAX_FILE_BYTES} bytes"));
        }
        if append {
            self.filesystem.append(&path, data).await
        } else {
            self.filesystem.write_atomic(&path, data).await
        }
        .map_err(fs_failure)
    }

    /// Entry names in `path`, sorted, directories ending in `/`, as a JSON array.
    pub(crate) async fn list(&self, path: &str) -> Result<String, Failure> {
        let dir = self.file(path)?;
        let mut names: Vec<String> = match self.filesystem.read_dir(&dir).await {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| {
                    let mut name = String::from(entry.file_name());
                    if entry.metadata().is_dir() {
                        name.push('/');
                    }
                    name
                })
                .collect(),
            // An app that never wrote a file has an empty root.
            Err(FsError::NotFound) if path.split('/').all(str::is_empty) => Vec::new(),
            Err(error) => return Err(fs_failure(error)),
        };
        names.sort_unstable();
        serde_json::to_string(&names).map_err(|_| String::from("storage failed"))
    }

    pub(crate) async fn remove(&self, path: &str) -> Result<(), Failure> {
        let path = self.file_name(path)?;
        self.filesystem.remove(&path).await.map_err(fs_failure)
    }

    pub(crate) async fn exists(&self, path: &str) -> Result<bool, Failure> {
        let path = self.file(path)?;
        self.filesystem.exists(&path).await.map_err(fs_failure)
    }

    pub(crate) async fn mkdir(&self, path: &str) -> Result<(), Failure> {
        let path = self.file_name(path)?;
        self.filesystem
            .create_dir_all(&path)
            .await
            .map_err(fs_failure)
    }
}
