//! Resource-image construction tests.

#![allow(clippy::expect_used)]

use std::error::Error;
use std::fs;

use barracuda_system_image::{
    build_directory as build_directory_with_filesystem,
    build_workspace as build_workspace_with_filesystem, ImageBuildError, ResourcesFilesystem,
};
use barracuda_vfs::{FsError, MountOptions, Vfs};
use barracuda_vfs_fat::FatFs;
use barracuda_vfs_littlefs::{LittleFs, PartitionStorage};
use embedded_io::ErrorType as IoErrorType;
use embedded_io_async::{Read, Seek, Write};
use tempfile::tempdir;

const SECTOR_SIZE: usize = 512;
const CAPACITY: usize = 512 * 1024;

fn build_directory(source: &std::path::Path, capacity: usize) -> Result<Vec<u8>, ImageBuildError> {
    build_directory_with_filesystem(source, capacity, ResourcesFilesystem::FatFs)
}

fn build_workspace(
    workspace: &std::path::Path,
    capacity: usize,
) -> Result<Vec<u8>, ImageBuildError> {
    build_workspace_with_filesystem(workspace, capacity, ResourcesFilesystem::FatFs)
}

struct FatImageDisk {
    bytes: Vec<u8>,
    cursor: usize,
}

impl IoErrorType for FatImageDisk {
    type Error = embedded_io::ErrorKind;
}

impl Read for FatImageDisk {
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let amount = output
            .len()
            .min(self.bytes.len().saturating_sub(self.cursor));
        output[..amount].copy_from_slice(&self.bytes[self.cursor..self.cursor + amount]);
        self.cursor += amount;
        Ok(amount)
    }
}

impl Write for FatImageDisk {
    async fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        let end = self
            .cursor
            .checked_add(input.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        self.bytes[self.cursor..end].copy_from_slice(input);
        self.cursor = end;
        Ok(input.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for FatImageDisk {
    async fn seek(&mut self, position: embedded_io::SeekFrom) -> Result<u64, Self::Error> {
        let next = match position {
            embedded_io::SeekFrom::Start(offset) => offset as i128,
            embedded_io::SeekFrom::End(offset) => self.bytes.len() as i128 + offset as i128,
            embedded_io::SeekFrom::Current(offset) => self.cursor as i128 + offset as i128,
        };
        if next < 0 || next > self.bytes.len() as i128 {
            return Err(embedded_io::ErrorKind::InvalidInput);
        }
        self.cursor = next as usize;
        Ok(self.cursor as u64)
    }
}

fn mounted_fat(bytes: Vec<u8>) -> Vfs {
    futures_lite::future::block_on(async {
        let fat = FatFs::mount(FatImageDisk { bytes, cursor: 0 })
            .await
            .expect("mount generated FAT image");
        let mut vfs = Vfs::new();
        vfs.mount("/", fat.into_backend(), MountOptions::read_only())
            .await
            .expect("mount FAT backend");
        vfs
    })
}

#[derive(Debug)]
struct MemoryFlashError;

impl embedded_storage::nor_flash::NorFlashError for MemoryFlashError {
    fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
        embedded_storage::nor_flash::NorFlashErrorKind::Other
    }
}

struct MemoryFlash(Vec<u8>);

impl embedded_storage::nor_flash::ErrorType for MemoryFlash {
    type Error = MemoryFlashError;
}

impl embedded_storage::nor_flash::ReadNorFlash for MemoryFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let start = usize::try_from(offset).map_err(|_error| MemoryFlashError)?;
        let end = start.checked_add(bytes.len()).ok_or(MemoryFlashError)?;
        bytes.copy_from_slice(self.0.get(start..end).ok_or(MemoryFlashError)?);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.0.len()
    }
}

impl embedded_storage::nor_flash::NorFlash for MemoryFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let from = usize::try_from(from).map_err(|_error| MemoryFlashError)?;
        let to = usize::try_from(to).map_err(|_error| MemoryFlashError)?;
        self.0.get_mut(from..to).ok_or(MemoryFlashError)?.fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let start = usize::try_from(offset).map_err(|_error| MemoryFlashError)?;
        let end = start.checked_add(bytes.len()).ok_or(MemoryFlashError)?;
        self.0
            .get_mut(start..end)
            .ok_or(MemoryFlashError)?
            .copy_from_slice(bytes);
        Ok(())
    }
}

fn mounted_littlefs(bytes: Vec<u8>) -> Vfs {
    let storage = PartitionStorage::<
        MemoryFlash,
        generic_array::typenum::U128,
        generic_array::typenum::U8,
        128,
    >::new(MemoryFlash(bytes))
    .expect("LittleFS storage geometry");
    let littlefs = LittleFs::mount(storage).expect("mount generated LittleFS image");
    futures_lite::future::block_on(async {
        let mut vfs = Vfs::new();
        vfs.mount("/", littlefs.into_backend(), MountOptions::read_only())
            .await
            .expect("mount LittleFS backend");
        vfs
    })
}

fn read_file(filesystem: &Vfs, path: &str) -> Result<Vec<u8>, FsError> {
    futures_lite::future::block_on(filesystem.read(path))
}

#[test]
fn builds_an_image_mountable_by_the_runtime_vfs_backend() {
    let source = tempdir().expect("source directory");
    fs::create_dir_all(source.path().join("resources/profiles")).expect("nested directory");
    fs::write(source.path().join("resources/workflows.json"), b"[]\n").expect("workflow file");
    fs::write(
        source.path().join("resources/profiles/assistant.md"),
        b"Barracuda\n",
    )
    .expect("nested file");

    let image = build_directory(source.path(), CAPACITY).expect("build image");

    assert_eq!(image.len(), CAPACITY);
    let filesystem = mounted_fat(image);
    assert_eq!(
        read_file(&filesystem, "/resources/workflows.json").expect("workflow contents"),
        b"[]\n"
    );
    assert_eq!(
        read_file(&filesystem, "/resources/profiles/assistant.md").expect("nested contents"),
        b"Barracuda\n"
    );
    assert_eq!(
        futures_lite::future::block_on(filesystem.write("/resources/new", b"blocked")),
        Err(FsError::ReadOnly)
    );
}

#[test]
fn builds_littlefs_and_fatfs_images_from_the_same_resource_tree() {
    let source = tempdir().expect("source directory");
    fs::create_dir_all(source.path().join("plugins/agent/resources")).expect("resource directory");
    fs::write(
        source.path().join("plugins/agent/resources/workflows.json"),
        b"[]\n",
    )
    .expect("resource file");

    let fat = build_directory_with_filesystem(source.path(), CAPACITY, ResourcesFilesystem::FatFs)
        .expect("build FATFS image");
    let littlefs =
        build_directory_with_filesystem(source.path(), CAPACITY, ResourcesFilesystem::LittleFs)
            .expect("build LittleFS image");

    for filesystem in [mounted_fat(fat), mounted_littlefs(littlefs)] {
        assert_eq!(
            read_file(&filesystem, "/plugins/agent/resources/workflows.json")
                .expect("bundled resource"),
            b"[]\n"
        );
    }
}

#[test]
fn workspace_build_bundles_enabled_plugin_resources_under_the_manifest_id() {
    let workspace = tempdir().expect("workspace directory");
    let plugin = workspace.path().join("plugins/directory-name");
    fs::create_dir_all(plugin.join("filesystem/resources/nested")).expect("Plugin resource source");
    fs::write(
        plugin.join("plugin.toml"),
        "id = \"manifest-id\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
    )
    .expect("Plugin manifest");
    fs::write(
        plugin.join("filesystem/resources/nested/asset.txt"),
        b"bundled",
    )
    .expect("Plugin resource");

    let image = build_workspace(workspace.path(), CAPACITY).expect("build workspace image");
    let filesystem = mounted_fat(image);

    assert_eq!(
        futures_lite::future::block_on(filesystem.read("/plugins/manifest-id/nested/asset.txt"))
            .expect("bundled Plugin resource"),
        b"bundled"
    );
}

#[test]
fn workspace_build_merges_enabled_plugin_workspace_resources() {
    let workspace = tempdir().expect("workspace directory");
    for (directory, id, file, contents) in [
        ("first", "first-id", "first.txt", b"first".as_slice()),
        ("second", "second-id", "second.txt", b"second".as_slice()),
    ] {
        let plugin = workspace.path().join("plugins").join(directory);
        fs::create_dir_all(plugin.join("filesystem/workspace/resources/models"))
            .expect("shared Plugin resources");
        fs::write(
            plugin.join("plugin.toml"),
            format!("id = \"{id}\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n"),
        )
        .expect("Plugin manifest");
        fs::write(
            plugin
                .join("filesystem/workspace/resources/models")
                .join(file),
            contents,
        )
        .expect("shared Plugin resource");
    }

    for filesystem in [ResourcesFilesystem::FatFs, ResourcesFilesystem::LittleFs] {
        let image = build_workspace_with_filesystem(workspace.path(), CAPACITY, filesystem)
            .expect("build workspace image");
        let filesystem = match filesystem {
            ResourcesFilesystem::FatFs => mounted_fat(image),
            ResourcesFilesystem::LittleFs => mounted_littlefs(image),
        };

        assert_eq!(
            read_file(&filesystem, "/workspace/models/first.txt").expect("first shared resource"),
            b"first"
        );
        assert_eq!(
            read_file(&filesystem, "/workspace/models/second.txt").expect("second shared resource"),
            b"second"
        );
    }
}

#[test]
fn workspace_build_rejects_conflicting_plugin_workspace_resources() {
    let workspace = tempdir().expect("workspace directory");
    for directory in ["first", "second"] {
        let plugin = workspace.path().join("plugins").join(directory);
        fs::create_dir_all(plugin.join("filesystem/workspace/resources"))
            .expect("shared Plugin resources");
        fs::write(
            plugin.join("plugin.toml"),
            format!("id = \"{directory}\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n"),
        )
        .expect("Plugin manifest");
        fs::write(
            plugin.join("filesystem/workspace/resources/common.txt"),
            directory,
        )
        .expect("shared Plugin resource");
    }

    assert!(matches!(
        build_workspace(workspace.path(), CAPACITY),
        Err(ImageBuildError::DuplicateWorkspaceResource(path))
            if path == "/workspace/common.txt"
    ));
}

#[test]
fn workspace_build_rejects_file_directory_workspace_conflicts() {
    let workspace = tempdir().expect("workspace directory");
    let first = workspace.path().join("plugins/first");
    fs::create_dir_all(first.join("filesystem/workspace/resources"))
        .expect("first shared Plugin resources");
    fs::write(
        first.join("plugin.toml"),
        "id = \"first\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
    )
    .expect("first Plugin manifest");
    fs::write(first.join("filesystem/workspace/resources/common"), b"file")
        .expect("shared Plugin file");

    let second = workspace.path().join("plugins/second");
    fs::create_dir_all(second.join("filesystem/workspace/resources/common"))
        .expect("second shared Plugin resources");
    fs::write(
        second.join("plugin.toml"),
        "id = \"second\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
    )
    .expect("second Plugin manifest");
    fs::write(
        second.join("filesystem/workspace/resources/common/nested.txt"),
        b"nested",
    )
    .expect("nested shared Plugin file");

    assert!(matches!(
        build_workspace(workspace.path(), CAPACITY),
        Err(ImageBuildError::DuplicateWorkspaceResource(path)) if path == "/workspace/common"
    ));
}

#[test]
fn workspace_build_excludes_disabled_plugin_resources() {
    let workspace = tempdir().expect("workspace directory");
    let plugin = workspace.path().join("plugins/disabled");
    fs::create_dir_all(plugin.join("filesystem/resources")).expect("Plugin resources");
    fs::write(
        plugin.join("plugin.toml"),
        "id = \"disabled-id\"\ndepends-on = []\ndescription = \"Disabled Plugin.\"\n",
    )
    .expect("Plugin manifest");
    fs::write(plugin.join("filesystem/resources/asset.txt"), b"disabled").expect("Plugin resource");
    fs::create_dir_all(plugin.join("filesystem/workspace/resources"))
        .expect("shared Plugin resources");
    fs::write(
        plugin.join("filesystem/workspace/resources/shared.txt"),
        b"disabled",
    )
    .expect("shared Plugin resource");
    fs::create_dir_all(workspace.path().join(".barracuda")).expect("selection directory");
    fs::write(
        workspace.path().join(".barracuda/disabled-plugins"),
        "disabled\n",
    )
    .expect("disabled Plugin selection");

    let image = build_workspace(workspace.path(), CAPACITY).expect("build workspace image");
    let filesystem = mounted_fat(image);

    assert!(read_file(&filesystem, "/plugins/disabled-id/asset.txt").is_err());
    assert!(read_file(&filesystem, "/workspace/shared.txt").is_err());
}

#[test]
fn workspace_build_creates_a_resource_root_for_every_enabled_plugin() {
    let workspace = tempdir().expect("workspace directory");
    let plugin = workspace.path().join("plugins/demo");
    fs::create_dir_all(&plugin).expect("Plugin directory");
    fs::write(
        plugin.join("plugin.toml"),
        "id = \"demo\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
    )
    .expect("Plugin manifest");

    let filesystem =
        mounted_fat(build_workspace(workspace.path(), CAPACITY).expect("build workspace image"));
    let metadata = futures_lite::future::block_on(filesystem.metadata("/plugins/demo"))
        .expect("enabled Plugin resource root");

    assert!(metadata.is_dir());
}

#[test]
fn workspace_build_rejects_non_resource_plugin_filesystem_entries() {
    for entry in ["data", "cache", "media", "unknown"] {
        let workspace = tempdir().expect("workspace directory");
        let plugin = workspace.path().join("plugins/demo");
        fs::create_dir_all(plugin.join("filesystem").join(entry))
            .expect("invalid Plugin filesystem source");
        fs::write(
            plugin.join("plugin.toml"),
            "id = \"demo\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
        )
        .expect("Plugin manifest");

        assert!(matches!(
            build_workspace(workspace.path(), CAPACITY),
            Err(ImageBuildError::UnsupportedPluginFilesystemEntry(path))
                if path == plugin.join("filesystem").join(entry)
        ));
    }
}

#[test]
fn workspace_build_rejects_non_resource_workspace_entries() {
    for entry in ["data", "cache", "media", "unknown"] {
        let workspace = tempdir().expect("workspace directory");
        let plugin = workspace.path().join("plugins/demo");
        fs::create_dir_all(plugin.join("filesystem/workspace").join(entry))
            .expect("invalid Workspace filesystem source");
        fs::write(
            plugin.join("plugin.toml"),
            "id = \"demo\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
        )
        .expect("Plugin manifest");

        assert!(matches!(
            build_workspace(workspace.path(), CAPACITY),
            Err(ImageBuildError::UnsupportedPluginFilesystemEntry(path))
                if path == plugin.join("filesystem/workspace").join(entry)
        ));
    }
}

#[test]
fn workspace_build_rejects_duplicate_bundled_plugin_ids() {
    let workspace = tempdir().expect("workspace directory");
    for directory in ["first", "second"] {
        let plugin = workspace.path().join("plugins").join(directory);
        fs::create_dir_all(plugin.join("filesystem/resources")).expect("Plugin resources");
        fs::write(
            plugin.join("plugin.toml"),
            "id = \"shared\"\ndepends-on = []\ndescription = \"Test Plugin.\"\n",
        )
        .expect("Plugin manifest");
    }

    assert!(matches!(
        build_workspace(workspace.path(), CAPACITY),
        Err(ImageBuildError::DuplicatePluginId(id)) if id == "shared"
    ));
}

#[test]
fn workspace_build_reports_invalid_contributing_plugin_manifests() {
    let workspace = tempdir().expect("workspace directory");
    let plugin = workspace.path().join("plugins/demo");
    fs::create_dir_all(plugin.join("filesystem/resources")).expect("Plugin resources");
    let manifest = plugin.join("plugin.toml");
    fs::write(&manifest, "not valid TOML").expect("invalid Plugin manifest");

    assert!(matches!(
        build_workspace(workspace.path(), CAPACITY),
        Err(ImageBuildError::PluginManifest { path, .. }) if path == manifest
    ));
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
        build_directory(parent.path(), SECTOR_SIZE + 1),
        Err(ImageBuildError::InvalidCapacity(capacity)) if capacity == SECTOR_SIZE + 1
    ));
    assert!(matches!(
        build_directory(parent.path(), 0),
        Err(ImageBuildError::InvalidCapacity(0))
    ));
}

#[test]
fn supports_the_smallest_deployed_resource_partition() {
    let source = tempdir().expect("source directory");

    let image = build_directory(source.path(), 256 * 1024).expect("smallest deployed image");

    assert_eq!(image.len(), 256 * 1024);
}

#[test]
fn supports_every_deployed_resource_partition_size() {
    let source = tempdir().expect("source directory");
    for capacity in [256 * 1024, 320 * 1024, 384 * 1024, 0x16_0000] {
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
        path: String::from("/resources/workflows.json"),
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
