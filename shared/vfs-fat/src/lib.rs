#![no_std]

//! Asynchronous FAT backend for `barracuda-vfs`.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;

use barracuda_vfs::{
    Backend, BackendFile, BackendFuture, DirEntry, FileType, FsError, Metadata, OpenOptions,
    SeekFrom, VfsBackend,
};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_io::ErrorType;
use embedded_io_async::{Read, Seek, Write};
use hadris_fat::error::Error as FatError;
use hadris_fat::r#async::dir::DirectoryEntry;
use hadris_fat::r#async::format::{FatFormatOptions, FatVolumeFormatter};
use hadris_fat::r#async::{FatVolume, FatVolumeReadExt, FatVolumeWriteExt};
use hadris_io::r#async::FromEmbedded;

struct ErrorMapped<T>(T);

impl<T: ErrorType> ErrorType for ErrorMapped<T> {
    type Error = FsError;
}

impl<T: Read> Read for ErrorMapped<T> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buffer).await.map_err(map_device_error)
    }
}

impl<T: Write> Write for ErrorMapped<T> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buffer).await.map_err(map_device_error)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(map_device_error)
    }
}

impl<T: Seek> Seek for ErrorMapped<T> {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.0.seek(position).await.map_err(map_device_error)
    }
}

struct Physical<T> {
    io: Mutex<CriticalSectionRawMutex, ErrorMapped<T>>,
}

struct Device<T> {
    physical: Arc<Physical<T>>,
}

impl<T> Clone for Device<T> {
    fn clone(&self) -> Self {
        Self {
            physical: self.physical.clone(),
        }
    }
}

impl<T> ErrorType for Device<T> {
    type Error = FsError;
}

impl<T: Read> Read for Device<T> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.physical.io.lock().await.read(buffer).await
    }
}

impl<T: Write> Write for Device<T> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.physical.io.lock().await.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.physical.io.lock().await.flush().await
    }
}

impl<T: Seek> Seek for Device<T> {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.physical.io.lock().await.seek(position).await
    }
}

type Volume<T> = FatVolume<FromEmbedded<Device<T>>>;

/// A FAT12/16/32 backend with VFAT long-name support and genuine async I/O.
///
/// Writable handles buffer one file and commit it on explicit
/// [`embedded_io_async::Write::flush`]. Dropping a dirty writable handle does
/// not block and therefore cannot implicitly flush it.
pub struct FatFs<T>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    device: Device<T>,
    gate: Arc<Mutex<CriticalSectionRawMutex, ()>>,
    volume: Arc<Mutex<CriticalSectionRawMutex, Volume<T>>>,
}

impl<T> FatFs<T>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    /// Mounts an already-formatted FAT volume over asynchronous embedded I/O.
    pub async fn mount(device: T) -> Result<Self, FsError> {
        let device = erase_device(device);
        let volume = open_volume(device.clone()).await?;
        Ok(Self {
            device,
            gate: Arc::new(Mutex::new(())),
            volume: Arc::new(Mutex::new(volume)),
        })
    }

    /// Formats and mounts a FAT volume of `volume_size` bytes.
    pub async fn format(device: T, volume_size: u64) -> Result<Self, FsError> {
        let mut device = erase_device(device);
        device.seek(SeekFrom::Start(0)).await?;
        let volume = FatVolumeFormatter::format(
            FromEmbedded::new(device.clone()),
            FatFormatOptions::new(volume_size),
        )
        .await
        .map_err(map_fat_error)?;
        Ok(Self {
            device,
            gate: Arc::new(Mutex::new(())),
            volume: Arc::new(Mutex::new(volume)),
        })
    }

    /// Erases this backend for mounting into a VFS namespace.
    pub fn into_backend(self) -> Backend {
        Backend::new(self)
    }

    async fn read_all(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let _gate = self.gate.lock().await;
        let volume = self.volume.lock().await;
        let entry = volume.open_path(path).await.map_err(map_fat_error)?;
        if !entry.is_file() {
            return Err(FsError::IsDirectory);
        }
        let size = usize::try_from(entry.len()).map_err(|_| FsError::Io)?;
        let mut bytes = vec![0; size];
        let mut reader = volume.read_file(&entry).map_err(map_fat_error)?;
        let mut filled = 0;
        while filled < bytes.len() {
            let read = reader
                .read(&mut bytes[filled..])
                .await
                .map_err(map_fat_error)?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        bytes.truncate(filled);
        Ok(bytes)
    }

    async fn ensure_file(&self, path: &str, exclusive: bool) -> Result<(), FsError> {
        let _gate = self.gate.lock().await;
        let volume = self.volume.lock().await;
        match volume.open_path(path).await {
            Ok(_) if exclusive => Err(FsError::AlreadyExists),
            Ok(entry) if entry.is_directory() => Err(FsError::IsDirectory),
            Ok(_) => Ok(()),
            Err(FatError::EntryNotFound) => {
                let (parent, name) = split_parent(path)?;
                let parent = open_dir(&volume, parent).await?;
                volume
                    .create_file(&parent, name)
                    .await
                    .map_err(map_fat_error)?;
                Ok(())
            }
            Err(error) => Err(map_fat_error(error)),
        }
    }
}

impl<T> VfsBackend for FatFs<T>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    fn open<'a>(
        &'a self,
        path: &'a str,
        options: &'a OpenOptions,
    ) -> BackendFuture<'a, Result<Box<dyn BackendFile>, FsError>> {
        Box::pin(async move {
            let exists = match self.metadata(path).await {
                Ok(metadata) if metadata.is_dir() => return Err(FsError::IsDirectory),
                Ok(_) => true,
                Err(FsError::NotFound) => false,
                Err(error) => return Err(error),
            };
            if !exists && !options.should_create() {
                return Err(FsError::NotFound);
            }

            let mode = if options.can_write() {
                self.ensure_file(path, options.is_create_new()).await?;
                let bytes = if exists && !options.should_truncate() {
                    self.read_all(path).await?
                } else {
                    Vec::new()
                };
                if options.should_truncate() {
                    commit_bytes(&self.device, &self.gate, path, &bytes).await?;
                }
                FileMode::Buffered {
                    bytes,
                    readable: options.can_read(),
                    append: options.is_append(),
                    dirty: false,
                }
            } else {
                FileMode::Reader
            };
            let cursor = match &mode {
                FileMode::Buffered { bytes, append, .. } if *append => bytes.len() as u64,
                _ => 0,
            };
            Ok(Box::new(FatFile {
                path: path.to_string(),
                cursor,
                device: self.device.clone(),
                gate: self.gate.clone(),
                mode,
            }) as Box<dyn BackendFile>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Metadata, FsError>> {
        Box::pin(async move {
            if path == "/" {
                return Ok(Metadata::new(FileType::Directory, 0));
            }
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let entry = volume.open_path(path).await.map_err(map_fat_error)?;
            Ok(entry_metadata(&entry))
        })
    }

    fn read_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<Vec<DirEntry>, FsError>> {
        Box::pin(async move {
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let directory = open_dir(&volume, path).await?;
            let mut iterator = directory.entries();
            let mut entries = Vec::new();
            while let Some(entry) = iterator.next_entry().await {
                let DirectoryEntry::Entry(entry) = entry.map_err(map_fat_error)?;
                let name = entry.name();
                if name == "." || name == ".." {
                    continue;
                }
                entries.push(DirEntry::new(name.into_owned(), entry_metadata(&entry)));
            }
            Ok(entries)
        })
    }

    fn create_dir_all<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            if path == "/" {
                return Ok(());
            }
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let mut directory = volume.root_dir();
            for component in path.split('/').filter(|component| !component.is_empty()) {
                directory = match directory.find(component).await.map_err(map_fat_error)? {
                    Some(entry) if entry.is_directory() => {
                        directory.open_entry(&entry).map_err(map_fat_error)?
                    }
                    Some(_) => return Err(FsError::NotDirectory),
                    None => volume
                        .create_dir(&directory, component)
                        .await
                        .map_err(map_fat_error)?,
                };
            }
            Ok(())
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let entry = volume.open_path(path).await.map_err(map_fat_error)?;
            if entry.is_directory() {
                return Err(FsError::IsDirectory);
            }
            volume.delete(&entry).await.map_err(map_fat_error)
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let entry = volume.open_path(path).await.map_err(map_fat_error)?;
            if !entry.is_directory() {
                return Err(FsError::NotDirectory);
            }
            volume.delete(&entry).await.map_err(map_fat_error)
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BackendFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let _gate = self.gate.lock().await;
            let volume = self.volume.lock().await;
            let entry = volume.open_path(from).await.map_err(map_fat_error)?;
            let (parent, name) = split_parent(to)?;
            let parent = open_dir(&volume, parent).await?;
            volume
                .rename(&entry, &parent, name)
                .await
                .map(|_| ())
                .map_err(map_fat_error)
        })
    }
}

enum FileMode {
    Reader,
    Buffered {
        bytes: Vec<u8>,
        readable: bool,
        append: bool,
        dirty: bool,
    },
}

struct FatFile<T> {
    path: String,
    cursor: u64,
    device: Device<T>,
    gate: Arc<Mutex<CriticalSectionRawMutex, ()>>,
    mode: FileMode,
}

impl<T> FatFile<T>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    async fn commit(&mut self) -> Result<(), FsError> {
        if let FileMode::Buffered { bytes, dirty, .. } = &mut self.mode {
            if *dirty {
                commit_bytes(&self.device, &self.gate, &self.path, bytes).await?;
                *dirty = false;
            }
        }
        Ok(())
    }

    fn len(&self) -> Option<u64> {
        match &self.mode {
            FileMode::Reader => None,
            FileMode::Buffered { bytes, .. } => Some(bytes.len() as u64),
        }
    }
}

impl<T> BackendFile for FatFile<T>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            let read = match &mut self.mode {
                FileMode::Reader => {
                    read_range(&self.device, &self.gate, &self.path, self.cursor, buffer).await?
                }
                FileMode::Buffered {
                    bytes, readable, ..
                } => {
                    if !*readable {
                        return Err(FsError::PermissionDenied);
                    }
                    let start = usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?;
                    if start >= bytes.len() {
                        return Ok(0);
                    }
                    let amount = buffer.len().min(bytes.len() - start);
                    buffer[..amount].copy_from_slice(&bytes[start..start + amount]);
                    amount
                }
            };
            self.cursor = self.cursor.checked_add(read as u64).ok_or(FsError::Io)?;
            Ok(read)
        })
    }

    fn write<'a>(&'a mut self, buffer: &'a [u8]) -> BackendFuture<'a, Result<usize, FsError>> {
        Box::pin(async move {
            let FileMode::Buffered {
                bytes,
                append,
                dirty,
                ..
            } = &mut self.mode
            else {
                return Err(FsError::PermissionDenied);
            };
            let start = if *append {
                bytes.len()
            } else {
                usize::try_from(self.cursor).map_err(|_| FsError::InvalidInput)?
            };
            let end = start.checked_add(buffer.len()).ok_or(FsError::Io)?;
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(buffer);
            *dirty = true;
            self.cursor = end as u64;
            Ok(buffer.len())
        })
    }

    fn flush(&mut self) -> BackendFuture<'_, Result<(), FsError>> {
        Box::pin(async move { self.commit().await })
    }

    fn seek(&mut self, position: SeekFrom) -> BackendFuture<'_, Result<u64, FsError>> {
        Box::pin(async move {
            let length = match self.len() {
                Some(length) => i128::from(length),
                None => i128::from(file_len(&self.device, &self.gate, &self.path).await?),
            };
            self.cursor = seek_target(self.cursor, length, position)? as u64;
            Ok(self.cursor)
        })
    }
}

fn erase_device<T>(device: T) -> Device<T> {
    Device {
        physical: Arc::new(Physical {
            io: Mutex::new(ErrorMapped(device)),
        }),
    }
}

async fn read_range<T>(
    device: &Device<T>,
    gate: &Mutex<CriticalSectionRawMutex, ()>,
    path: &str,
    offset: u64,
    buffer: &mut [u8],
) -> Result<usize, FsError>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    let _guard = gate.lock().await;
    let volume = open_volume(device.clone()).await?;
    let mut reader = volume.open_file_path(path).await.map_err(map_fat_error)?;
    let mut remaining = offset;
    let mut scratch = [0_u8; 1024];
    while remaining != 0 {
        let amount = cmp::min(remaining, scratch.len() as u64) as usize;
        let read = reader
            .read(&mut scratch[..amount])
            .await
            .map_err(map_fat_error)?;
        if read == 0 {
            return Ok(0);
        }
        remaining -= read as u64;
    }
    reader.read(buffer).await.map_err(map_fat_error)
}

async fn file_len<T>(
    device: &Device<T>,
    gate: &Mutex<CriticalSectionRawMutex, ()>,
    path: &str,
) -> Result<u64, FsError>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    let _guard = gate.lock().await;
    let volume = open_volume(device.clone()).await?;
    let entry = volume.open_path(path).await.map_err(map_fat_error)?;
    Ok(entry.len())
}

async fn commit_bytes<T>(
    device: &Device<T>,
    gate: &Mutex<CriticalSectionRawMutex, ()>,
    path: &str,
    bytes: &[u8],
) -> Result<(), FsError>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    let _guard = gate.lock().await;
    let volume = open_volume(device.clone()).await?;
    let entry = match volume.open_path(path).await {
        Ok(entry) => {
            volume.truncate(&entry, 0).await.map_err(map_fat_error)?;
            volume.open_path(path).await.map_err(map_fat_error)?
        }
        Err(FatError::EntryNotFound) => {
            let (parent, name) = split_parent(path)?;
            let parent = open_dir(&volume, parent).await?;
            volume
                .create_file(&parent, name)
                .await
                .map_err(map_fat_error)?
        }
        Err(error) => return Err(map_fat_error(error)),
    };
    if !bytes.is_empty() {
        let mut writer = volume.write_file(&entry).map_err(map_fat_error)?;
        let mut written = 0;
        while written < bytes.len() {
            let amount = writer
                .write(&bytes[written..])
                .await
                .map_err(map_fat_error)?;
            if amount == 0 {
                return Err(FsError::Io);
            }
            written += amount;
        }
        writer.finish().await.map_err(map_fat_error)?;
    }
    let mut device = device.clone();
    device.flush().await
}

async fn open_dir<'a, T>(
    volume: &'a Volume<T>,
    path: &str,
) -> Result<hadris_fat::r#async::FatDir<'a, FromEmbedded<Device<T>>>, FsError>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    if path == "/" {
        Ok(volume.root_dir())
    } else {
        volume.open_dir_path(path).await.map_err(map_fat_error)
    }
}

async fn open_volume<T>(mut device: Device<T>) -> Result<Volume<T>, FsError>
where
    T: ErrorType + Read + Write + Seek + Send + 'static,
    T::Error: embedded_io::Error,
{
    device.seek(SeekFrom::Start(0)).await?;
    FatVolume::open(FromEmbedded::new(device))
        .await
        .map_err(map_fat_error)
}

fn split_parent(path: &str) -> Result<(&str, &str), FsError> {
    let index = path.rfind('/').ok_or(FsError::InvalidPath)?;
    let name = path.get(index + 1..).ok_or(FsError::InvalidPath)?;
    if name.is_empty() {
        return Err(FsError::InvalidPath);
    }
    let parent = if index == 0 {
        "/"
    } else {
        path.get(..index).ok_or(FsError::InvalidPath)?
    };
    Ok((parent, name))
}

fn entry_metadata(entry: &hadris_fat::r#async::FileEntry) -> Metadata {
    Metadata::new(
        if entry.is_directory() {
            FileType::Directory
        } else {
            FileType::File
        },
        entry.len(),
    )
}

fn seek_target(current: u64, length: i128, position: SeekFrom) -> Result<i128, FsError> {
    let next = match position {
        SeekFrom::Start(offset) => i128::from(offset),
        SeekFrom::End(offset) => length
            .checked_add(i128::from(offset))
            .ok_or(FsError::InvalidInput)?,
        SeekFrom::Current(offset) => i128::from(current)
            .checked_add(i128::from(offset))
            .ok_or(FsError::InvalidInput)?,
    };
    if next < 0 || next > i128::from(u64::MAX) {
        Err(FsError::InvalidInput)
    } else {
        Ok(next)
    }
}

fn map_device_error(error: impl embedded_io::Error) -> FsError {
    match error.kind() {
        embedded_io::ErrorKind::InvalidInput => FsError::InvalidInput,
        embedded_io::ErrorKind::PermissionDenied => FsError::PermissionDenied,
        embedded_io::ErrorKind::NotConnected => FsError::MediaRemoved,
        _ => FsError::Io,
    }
}

fn map_fat_error(error: FatError) -> FsError {
    match error {
        FatError::Io(error) => map_device_error(embedded_io::ErrorKind::from(error.kind())),
        FatError::IoContext { source, .. } => {
            map_device_error(embedded_io::ErrorKind::from(source.kind()))
        }
        FatError::EntryNotFound => FsError::NotFound,
        FatError::NotAFile => FsError::IsDirectory,
        FatError::NotADirectory => FsError::NotDirectory,
        FatError::InvalidPath | FatError::InvalidFilename | FatError::InvalidShortFilename => {
            FsError::InvalidPath
        }
        FatError::AlreadyExists => FsError::AlreadyExists,
        FatError::DirectoryNotEmpty => FsError::DirectoryNotEmpty,
        FatError::NoFreeSpace | FatError::DirectoryFull => FsError::Io,
        _ => FsError::Io,
    }
}
