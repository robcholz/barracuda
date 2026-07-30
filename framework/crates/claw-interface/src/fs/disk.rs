use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::{ClawFile, ClawFs, FsError};

fn map_io(error: std::io::Error) -> FsError {
    FsError::from(error)
}

#[derive(Debug, Clone, Default)]
struct DiskConfig {
    base: Option<PathBuf>,
    #[cfg(feature = "diskfs-pretty")]
    pretty_json: bool,
}

/// Host [`ClawFs`] over `std::fs`.
///
/// Two addressing modes share one durable write discipline (write to a `.tmp`
/// sibling then `rename`, creating parent directories as needed):
/// - [`absolute`](DiskFs::absolute): paths are used verbatim. Used by the
///   host CLIs and conversation-memory tests that already hold absolute paths.
/// - [`rooted`](DiskFs::rooted): paths are joined onto a base directory (a
///   leading `/` is stripped so absolute-looking virtual paths stay inside the
///   root), keeping on-disk fixtures portable. Used by the skill-registry
///   tests.
#[derive(Debug, Default)]
pub struct DiskFs {
    config: DiskConfig,
}

impl DiskFs {
    /// Verbatim-path mode: the trait `path` is the on-disk path.
    pub fn absolute() -> Self {
        Self::default()
    }

    /// Rooted mode: the trait `path` is joined onto `base` (leading `/`
    /// stripped) so virtual paths resolve inside the root.
    pub fn rooted(base: impl Into<PathBuf>) -> Self {
        Self {
            config: DiskConfig {
                base: Some(base.into()),
                #[cfg(feature = "diskfs-pretty")]
                pretty_json: false,
            },
        }
    }

    /// Pretty-print `.json` writes so the on-disk files are readable when
    /// inspecting a test's output directory. Off by default.
    #[cfg(feature = "diskfs-pretty")]
    pub fn with_pretty_json(mut self, enabled: bool) -> Self {
        self.config.pretty_json = enabled;
        self
    }

    fn resolve(&self, path: &str) -> PathBuf {
        match &self.config.base {
            Some(base) => base.join(path.trim_start_matches('/')),
            None => PathBuf::from(path),
        }
    }

    /// Ensure the parent directory of `full` exists before a write.
    fn ensure_parent(full: &std::path::Path) -> Result<(), FsError> {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(FsError::from)?;
        }
        Ok(())
    }
}

/// An open handle over a [`std::fs::File`].
pub struct DiskFile {
    file: std::fs::File,
}

impl ClawFile for DiskFile {
    fn read_to_end(&mut self) -> Result<Vec<u8>, FsError> {
        let mut buffer = Vec::new();
        self.file.read_to_end(&mut buffer).map_err(FsError::from)?;
        Ok(buffer)
    }

    fn read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(FsError::from)?;
        let mut buffer = vec![0u8; len];
        self.file.read_exact(&mut buffer).map_err(FsError::from)?;
        Ok(buffer)
    }

    fn size(&self) -> Result<u64, FsError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(map_io)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), FsError> {
        self.file.write_all(data).map_err(FsError::from)
    }
}

impl ClawFs for DiskFs {
    type File = DiskFile;

    fn open(&self, path: &str) -> Result<Self::File, FsError> {
        std::fs::File::open(self.resolve(path))
            .map(|file| DiskFile { file })
            .map_err(map_io)
    }

    fn create(&self, path: &str) -> Result<Self::File, FsError> {
        let full = self.resolve(path);
        Self::ensure_parent(&full)?;
        std::fs::File::create(&full)
            .map(|file| DiskFile { file })
            .map_err(FsError::from)
    }

    fn open_append(&self, path: &str) -> Result<Self::File, FsError> {
        let full = self.resolve(path);
        Self::ensure_parent(&full)?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&full)
            .map(|file| DiskFile { file })
            .map_err(FsError::from)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        std::fs::rename(self.resolve(from), self.resolve(to)).map_err(map_io)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), FsError> {
        std::fs::create_dir_all(self.resolve(path)).map_err(FsError::from)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).exists()
    }

    fn remove(&self, path: &str) -> Result<(), FsError> {
        let full = self.resolve(path);
        let result = match std::fs::symlink_metadata(&full) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir(full),
            Ok(_) => std::fs::remove_file(full),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(FsError::from(error)),
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(FsError::from(error)),
        }
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let entries = std::fs::read_dir(self.resolve(path)).map_err(map_io)?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(FsError::from)?;
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    /// Byte length via `stat`, avoiding an open just to read metadata.
    fn len(&self, path: &str) -> Result<u64, FsError> {
        std::fs::metadata(self.resolve(path))
            .map(|metadata| metadata.len())
            .map_err(map_io)
    }

    /// Durable whole-file replace: write a `.tmp` sibling, then `rename` it
    /// over the target. Overrides the trait default to add optional
    /// pretty-printing of `.json` payloads (the `diskfs-pretty` feature).
    fn write_atomic(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        let full = self.resolve(path);
        Self::ensure_parent(&full)?;
        #[cfg(feature = "diskfs-pretty")]
        let payload = self.render(path, data);
        #[cfg(not(feature = "diskfs-pretty"))]
        let payload = std::borrow::Cow::Borrowed(data);
        let mut tmp = full.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, payload.as_ref()).map_err(FsError::from)?;
        std::fs::rename(&tmp, &full).map_err(FsError::from)
    }
}

#[cfg(feature = "diskfs-pretty")]
impl DiskFs {
    /// Pretty-print `.json` payloads when enabled; otherwise pass through.
    fn render<'data>(&self, path: &str, data: &'data [u8]) -> std::borrow::Cow<'data, [u8]> {
        if self.config.pretty_json && path.ends_with(".json") {
            match serde_json::from_slice::<serde_json::Value>(data)
                .ok()
                .and_then(|value| serde_json::to_vec_pretty(&value).ok())
            {
                Some(pretty) => std::borrow::Cow::Owned(pretty),
                None => std::borrow::Cow::Borrowed(data),
            }
        } else {
            std::borrow::Cow::Borrowed(data)
        }
    }
}
