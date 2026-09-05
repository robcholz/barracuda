//! LittleFS image construction tests.

#![allow(clippy::expect_used)]

use std::error::Error;
use std::fs;

use barracuda_system_image::{build_directory, ImageBuildError};
use barracuda_vfs_littlefs::{LittleFs, PartitionStorage};
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use generic_array::typenum::{U128, U8};
use tempfile::tempdir;

const BLOCK_SIZE: usize = 4096;
const BLOCK_COUNT: usize = 16;
const CAPACITY: usize = BLOCK_SIZE * BLOCK_COUNT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestFlashError {
    OutOfBounds,
    NotAligned,
}

impl NorFlashError for TestFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

struct TestFlash {
    bytes: Vec<u8>,
}

impl TestFlash {
    fn range(
        &self,
        offset: u32,
        length: usize,
        alignment: usize,
    ) -> Result<std::ops::Range<usize>, TestFlashError> {
        let start = usize::try_from(offset).map_err(|_error| TestFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(TestFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(TestFlashError::OutOfBounds)?;
        Ok(start..end)
    }
}

impl ErrorType for TestFlash {
    type Error = TestFlashError;
}

impl ReadNorFlash for TestFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let source = self
            .bytes
            .get(self.range(offset, bytes.len(), Self::READ_SIZE)?)
            .ok_or(TestFlashError::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for TestFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = BLOCK_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = usize::try_from(to.checked_sub(from).ok_or(TestFlashError::OutOfBounds)?)
            .map_err(|_error| TestFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        self.bytes
            .get_mut(range)
            .ok_or(TestFlashError::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let destination = self
            .bytes
            .get_mut(range)
            .ok_or(TestFlashError::OutOfBounds)?;
        for (destination, source) in destination.iter_mut().zip(bytes) {
            *destination &= *source;
        }
        Ok(())
    }
}

fn mounted(bytes: Vec<u8>) -> LittleFs<PartitionStorage<TestFlash, U128, U8, BLOCK_COUNT>> {
    let storage = PartitionStorage::new(TestFlash { bytes }).expect("valid image geometry");
    LittleFs::mount(storage).expect("mount generated image")
}

#[test]
fn builds_an_image_mountable_by_the_runtime_vfs_backend() {
    let source = tempdir().expect("source directory");
    fs::create_dir_all(source.path().join("system/profiles")).expect("nested directory");
    fs::write(source.path().join("system/workflows.json"), b"[]\n").expect("workflow file");
    fs::write(
        source.path().join("system/profiles/assistant.md"),
        b"Barracuda\n",
    )
    .expect("nested file");

    let image = build_directory(source.path(), CAPACITY).expect("build image");

    assert_eq!(image.len(), CAPACITY);
    let filesystem = mounted(image);
    assert_eq!(
        filesystem
            .read_file("/system/workflows.json")
            .expect("workflow contents"),
        b"[]\n"
    );
    assert_eq!(
        filesystem
            .read_file("/system/profiles/assistant.md")
            .expect("nested contents"),
        b"Barracuda\n"
    );
}

#[test]
fn identical_source_trees_produce_identical_images() {
    let source = tempdir().expect("source directory");
    fs::create_dir_all(source.path().join("z-last")).expect("nested directory");
    fs::write(source.path().join("z-last/value"), b"z").expect("last file");
    fs::write(source.path().join("a-first"), b"a").expect("first file");

    let first = build_directory(source.path(), CAPACITY).expect("first image");
    let second = build_directory(source.path(), CAPACITY).expect("second image");

    assert_eq!(first, second);
}

#[test]
fn rejects_invalid_sources_and_capacities() {
    let parent = tempdir().expect("temporary directory");
    let missing = parent.path().join("missing");
    assert!(matches!(
        build_directory(&missing, CAPACITY),
        Err(ImageBuildError::SourceRead { path, .. }) if path == missing
    ));

    let file = parent.path().join("file");
    fs::write(&file, b"not a directory").expect("source file");
    assert!(matches!(
        build_directory(&file, CAPACITY),
        Err(ImageBuildError::SourceNotDirectory(path)) if path == file
    ));
    assert!(matches!(
        build_directory(parent.path(), BLOCK_SIZE + 1),
        Err(ImageBuildError::InvalidCapacity(capacity)) if capacity == BLOCK_SIZE + 1
    ));
    assert!(matches!(
        build_directory(parent.path(), BLOCK_SIZE),
        Err(ImageBuildError::InvalidCapacity(BLOCK_SIZE))
    ));
}

#[test]
fn supports_the_minimum_aligned_capacity() {
    let source = tempdir().expect("source directory");

    let image = build_directory(source.path(), BLOCK_SIZE * 2).expect("minimum image");

    assert_eq!(image.len(), BLOCK_SIZE * 2);
}

#[test]
fn supports_every_runtime_geometry_tier() {
    let source = tempdir().expect("source directory");
    for block_count in [
        4, 8, 16, 32, 64, 128, 256, 384, 512, 768, 1_024, 1_536, 2_048, 3_072, 4_096,
    ] {
        let capacity = BLOCK_SIZE * block_count;
        let image = build_directory(source.path(), capacity).expect("supported geometry");
        assert_eq!(image.len(), capacity);
    }
}

#[test]
fn build_errors_identify_the_failing_boundary() {
    let path = std::path::PathBuf::from("source");
    let source_error = ImageBuildError::SourceRead {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    };
    assert!(source_error.to_string().contains("failed to read `source`"));
    assert!(source_error.source().is_some());
    assert!(ImageBuildError::SourceNotDirectory(path.clone())
        .to_string()
        .contains("not a directory"));
    assert!(ImageBuildError::UnsupportedEntry(path.clone())
        .to_string()
        .contains("unsupported entry"));
    assert!(ImageBuildError::InvalidPath(path)
        .to_string()
        .contains("cannot be represented"));
    assert!(ImageBuildError::Filesystem {
        path: String::from("/system/workflows.json"),
    }
    .to_string()
    .contains("failed to populate"));
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_links_in_the_source_tree() {
    use std::os::unix::fs::symlink;

    let source = tempdir().expect("source directory");
    fs::write(source.path().join("real"), b"content").expect("source file");
    let link = source.path().join("link");
    symlink("real", &link).expect("source symlink");

    let error = build_directory(source.path(), CAPACITY).expect_err("symlink must fail");

    assert!(matches!(error, ImageBuildError::UnsupportedEntry(path) if path == link));
}
