//! Host-side construction of Barracuda resource images.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use barracuda_board_config::{parse, read_selected_board};
use barracuda_platform_config::{resolve_board_platform, FlashDriver, LayoutDriver};
use barracuda_plugin::manifest::{parse as parse_plugin_manifest, ManifestError};
use barracuda_vfs::{MountOptions, Vfs};
use barracuda_vfs_fat::FatFs;
use embedded_io::{ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use esp_idf_part::{DataType, Flags, PartitionTable, SubType};
use generic_array::typenum::{U128, U8};
use littlefs2::driver::Storage;
use littlefs2::fs::Filesystem;
use littlefs2::path::PathBuf as LittlePathBuf;
use serde::Deserialize;

mod command;
mod flash;

use flash::{FlashRequest, PlatformFlash};

/// Filesystem format selected by the native `resources` partition model.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum ResourcesFilesystem {
    /// FAT12/16/32, selected according to partition capacity.
    #[serde(rename = "fatfs")]
    FatFs,
    /// LittleFS over the native erase geometry.
    #[serde(rename = "littlefs")]
    LittleFs,
}

/// Workspace-relative output path for the raw resource image.
pub const IMAGE_OUTPUT: &str = "target/barracuda-system.img";

const FAT_SECTOR_SIZE: usize = 512;

/// Selected Board region assigned to bundled read-only resources.
#[derive(Debug, PartialEq, Eq)]
pub struct ResourcesRegion {
    board: String,
    offset: u64,
    size: usize,
    filesystem: ResourcesFilesystem,
    platform_flash: PlatformFlash,
}

impl ResourcesRegion {
    /// Returns the selected Board name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the native flash offset or address reported by the Board layout.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the resource region capacity in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the filesystem format selected for this resource image.
    #[must_use]
    pub const fn filesystem(&self) -> ResourcesFilesystem {
        self.filesystem
    }
}

/// Description of one completed System image build.
#[derive(Debug, PartialEq, Eq)]
pub struct BuiltImage {
    board: String,
    output: PathBuf,
    offset: u64,
    size: usize,
    filesystem: ResourcesFilesystem,
}

/// Description of one completed resource-image flash.
#[derive(Debug, PartialEq, Eq)]
pub struct FlashedImage {
    board: String,
    image: PathBuf,
    destination: String,
    offset: u64,
    size: usize,
    filesystem: ResourcesFilesystem,
}

impl FlashedImage {
    /// Returns the selected Board name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the raw image that was flashed.
    #[must_use]
    pub fn image(&self) -> &Path {
        &self.image
    }

    /// Returns the Platform flash destination or tool name.
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// Returns the selected Board's native resource-region offset or address.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the number of flashed bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the filesystem format contained by the flashed image.
    #[must_use]
    pub const fn filesystem(&self) -> ResourcesFilesystem {
        self.filesystem
    }
}

impl BuiltImage {
    /// Returns the selected Board name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the raw image output path.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// Returns the selected Board's native resource-region offset or address.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the raw image size in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Returns the filesystem format written into the image.
    #[must_use]
    pub const fn filesystem(&self) -> ResourcesFilesystem {
        self.filesystem
    }
}

/// Failure while constructing a resource image.
#[derive(Debug)]
#[non_exhaustive]
pub enum ImageBuildError {
    /// The source directory or one of its entries could not be read.
    SourceRead {
        /// Host path that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// The source path exists but is not a directory.
    SourceNotDirectory(PathBuf),
    /// A source entry is neither a regular file nor a directory.
    UnsupportedEntry(PathBuf),
    /// A Plugin attempted to prebuild a mutable or unknown filesystem mount.
    UnsupportedPluginFilesystemEntry(PathBuf),
    /// A Plugin filesystem contribution has an invalid manifest.
    PluginManifest {
        /// Manifest path that could not be parsed.
        path: PathBuf,
        /// Manifest validation failure.
        source: ManifestError,
    },
    /// Two bundled Plugin directories declare the same stable identity.
    DuplicatePluginId(String),
    /// Two Plugins contribute incompatible entries at one shared Workspace path.
    DuplicateWorkspaceResource(String),
    /// A source path cannot be represented by the image path contract.
    InvalidPath(PathBuf),
    /// The image capacity is incompatible with the selected filesystem.
    InvalidCapacity(usize),
    /// The selected filesystem could not format or populate the image.
    Filesystem {
        /// Image-relative path being processed.
        path: String,
    },
}

impl fmt::Display for ImageBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceRead { path, source } => {
                write!(formatter, "failed to read `{}`: {source}", path.display())
            }
            Self::SourceNotDirectory(path) => {
                write!(
                    formatter,
                    "image source is not a directory: `{}`",
                    path.display()
                )
            }
            Self::UnsupportedEntry(path) => write!(
                formatter,
                "image source contains an unsupported entry: `{}`",
                path.display()
            ),
            Self::UnsupportedPluginFilesystemEntry(path) => write!(
                formatter,
                "Plugin filesystem may prebuild only `resources` or `workspace/resources`, found `{}`",
                path.display()
            ),
            Self::PluginManifest { path, source } => {
                write!(
                    formatter,
                    "invalid Plugin manifest `{}`: {source}",
                    path.display()
                )
            }
            Self::DuplicatePluginId(id) => {
                write!(formatter, "more than one bundled Plugin declares ID `{id}`")
            }
            Self::DuplicateWorkspaceResource(path) => write!(
                formatter,
                "more than one Plugin contributes Workspace resource `{path}`"
            ),
            Self::InvalidPath(path) => write!(
                formatter,
                "image path cannot be represented in the resource image: `{}`",
                path.display()
            ),
            Self::InvalidCapacity(capacity) => write!(
                formatter,
                "image capacity {capacity} is unsupported by the selected resource filesystem"
            ),
            Self::Filesystem { path } => {
                write!(formatter, "failed to populate resource image path `{path}`")
            }
        }
    }
}

impl std::error::Error for ImageBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRead { source, .. } => Some(source),
            Self::PluginManifest { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Builds enabled Plugin resources for the selected Board.
///
/// # Errors
///
/// Returns an error when Board selection or its native layout is invalid, the
/// source tree cannot be converted to the selected filesystem, or the output
/// cannot be written.
pub fn build_selected(workspace: &Path) -> Result<BuiltImage, String> {
    let region = selected_resources_region(workspace)?;
    let output = workspace.join(IMAGE_OUTPUT);
    let bytes = build_workspace(workspace, region.size, region.filesystem)
        .map_err(|error| error.to_string())?;
    write_image(&output, &bytes)?;
    Ok(BuiltImage {
        board: region.board,
        output,
        offset: region.offset,
        size: region.size,
        filesystem: region.filesystem,
    })
}

/// Flashes the built resource image into the selected Board's `resources` partition.
///
/// The selected Board determines both the native partition bounds and the
/// internal Platform flasher. This command never rebuilds the image.
///
/// # Errors
///
/// Returns an error when selection or native layout is invalid, the built
/// image is absent or stale, Platform configuration is invalid, or flashing
/// fails.
pub fn flash_selected(workspace: &Path) -> Result<FlashedImage, String> {
    let region = selected_resources_region(workspace)?;
    let image = workspace.join(IMAGE_OUTPUT);
    let destination = flash::flash(FlashRequest {
        workspace,
        image: &image,
        offset: region.offset,
        size: region.size,
        platform: &region.platform_flash,
    })?;
    Ok(FlashedImage {
        board: region.board,
        image,
        destination,
        offset: region.offset,
        size: region.size,
        filesystem: region.filesystem,
    })
}

/// Builds and then flashes the resource image for the selected Board.
///
/// # Errors
///
/// Returns an error when building the selected image fails or when the newly
/// built image cannot be flashed.
pub fn deploy_selected(workspace: &Path) -> Result<FlashedImage, String> {
    build_selected(workspace)?;
    flash_selected(workspace)
}

/// Resolves the read-only resource region from the selected Board's native layout.
///
/// # Errors
///
/// Returns an error when no Board is selected, the selected bundle is invalid,
/// or its native layout does not contain a read-only resource region.
pub fn selected_resources_region(workspace: &Path) -> Result<ResourcesRegion, String> {
    let board_name = read_selected_board(workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| String::from("no Board selected; run `cargo board select` first"))?;
    let bundle = workspace.join("boards/configs").join(&board_name);
    let board_path = bundle.join("board.yml");
    let board_yaml = read_text(&board_path, "Board definition")?;
    let board = parse(&board_yaml).map_err(|error| error.to_string())?;
    if board.name() != board_name {
        return Err(format!(
            "Board directory `{board_name}` declares Board `{}`",
            board.name()
        ));
    }
    let layout_path = bundle.join(board.native_layout().artifact());
    let layout = read_text(&layout_path, "native layout")?;
    let chip = board.hardware().chip();
    let platform = resolve_board_platform(
        workspace,
        chip,
        board.toolchain().map(|toolchain| toolchain.target()),
    )
    .map_err(|error| error.to_string())?;
    let (offset, size, capacity, filesystem) = match platform.system_image().layout() {
        LayoutDriver::FileRegions => {
            let (offset, size, capacity, filesystem) = file_layout_resources_region(&layout)?;
            (offset, size, Some(capacity), filesystem)
        }
        LayoutDriver::EspIdfPartitions => {
            let (offset, size, filesystem) = esp_resources_region(&layout)?;
            (offset, size, None, filesystem)
        }
        LayoutDriver::LinkerMemory => {
            let (offset, size, filesystem) = stm32_resources_region(&layout)?;
            (offset, size, None, filesystem)
        }
        LayoutDriver::Command(driver) => {
            let (offset, size, capacity, filesystem) =
                command_system_region(workspace, platform.directory(), &layout_path, chip, driver)?;
            (offset, size, capacity, filesystem)
        }
    };
    let platform_flash = match platform.system_image().flash() {
        FlashDriver::File {
            state_directory,
            flash_image,
        } => PlatformFlash::File {
            state_directory: state_directory.clone(),
            flash_image: flash_image.clone(),
            capacity: capacity.ok_or_else(|| {
                format!(
                    "Platform `{}` uses file flash but its layout driver reports no capacity",
                    platform.name()
                )
            })?,
        },
        FlashDriver::Command(driver) => PlatformFlash::Command {
            platform_directory: platform.directory().to_path_buf(),
            layout: layout_path,
            chip: chip.to_owned(),
            driver: driver.clone(),
        },
    };
    Ok(ResourcesRegion {
        board: board_name,
        offset,
        size,
        filesystem,
        platform_flash,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRegion {
    offset: u64,
    size: usize,
    #[serde(default)]
    capacity: Option<usize>,
    filesystem: ResourcesFilesystem,
}

fn command_system_region(
    workspace: &Path,
    platform: &Path,
    layout: &Path,
    chip: &str,
    driver: &barracuda_platform_config::CommandDriver,
) -> Result<(u64, usize, Option<usize>, ResourcesFilesystem), String> {
    let command = command::prepare(
        driver,
        command::DriverContext {
            workspace,
            platform,
            layout,
            chip,
            image: None,
            offset: None,
            size: None,
        },
    );
    let stdout = command::output(&command)?;
    let yaml = std::str::from_utf8(&stdout)
        .map_err(|error| format!("Platform layout command output is not UTF-8: {error}"))?;
    let mut documents = yaml_peg::serde::from_str::<CommandRegion>(yaml)
        .map_err(|error| format!("invalid Platform layout command output: {error}"))?;
    if documents.len() != 1 {
        return Err(format!(
            "Platform layout command must return one YAML document, found {}",
            documents.len()
        ));
    }
    let region = documents
        .pop()
        .ok_or_else(|| String::from("Platform layout command returned an empty document"))?;
    Ok((
        region.offset,
        region.size,
        region.capacity,
        region.filesystem,
    ))
}

/// Builds one raw resource image using the explicitly selected filesystem.
///
/// Entries are traversed in lexical order, so identical source trees produce
/// identical images. Symbolic links and special entries are rejected.
///
/// # Errors
///
/// Returns [`ImageBuildError`] when the source is invalid, the capacity is not
/// supported, or the selected filesystem cannot format or populate the image.
pub fn build_directory(
    source: &Path,
    capacity: usize,
    filesystem: ResourcesFilesystem,
) -> Result<Vec<u8>, ImageBuildError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|source_error| ImageBuildError::SourceRead {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    if !metadata.is_dir() {
        return Err(ImageBuildError::SourceNotDirectory(source.to_path_buf()));
    }
    let mut entries = Vec::new();
    collect_entries(source, source, "", &mut entries)?;
    build_entries(capacity, &entries, filesystem)
}

/// Builds a System image exclusively from enabled Plugin resources.
///
/// A file at `plugins/<directory>/filesystem/resources/<path>` is placed at
/// `/plugins/<manifest-id>/<path>`. A file at
/// `plugins/<directory>/filesystem/workspace/resources/<path>` is placed at
/// `/workspace/<path>`. Mutable Plugin mounts are never accepted as image
/// sources.
///
/// # Errors
///
/// Returns [`ImageBuildError`] when an image source, Plugin manifest, Plugin
/// filesystem contribution, or image geometry is invalid.
pub fn build_workspace(
    workspace: &Path,
    capacity: usize,
    filesystem: ResourcesFilesystem,
) -> Result<Vec<u8>, ImageBuildError> {
    let mut entries = Vec::new();
    collect_plugin_resources(workspace, &mut entries)?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    build_entries(capacity, &entries, filesystem)
}

fn build_entries(
    capacity: usize,
    entries: &[SourceEntry],
    filesystem: ResourcesFilesystem,
) -> Result<Vec<u8>, ImageBuildError> {
    match filesystem {
        ResourcesFilesystem::FatFs => build_fat_entries(capacity, entries),
        ResourcesFilesystem::LittleFs => build_littlefs_entries(capacity, entries),
    }
}

fn build_fat_entries(capacity: usize, entries: &[SourceEntry]) -> Result<Vec<u8>, ImageBuildError> {
    if capacity == 0 || capacity.checked_rem(FAT_SECTOR_SIZE) != Some(0) {
        return Err(ImageBuildError::InvalidCapacity(capacity));
    }
    let mut image = vec![0; capacity];
    fatfs::format_volume(
        std::io::Cursor::new(image.as_mut_slice()),
        fatfs::FormatVolumeOptions::new(),
    )
    .map_err(|_error| ImageBuildError::Filesystem {
        path: String::from("/"),
    })?;
    let bytes = Arc::new(Mutex::new(image));
    let disk = ImageDisk::new(bytes.clone());
    futures_lite::future::block_on(async {
        let fat = FatFs::mount(disk)
            .await
            .map_err(|_error| ImageBuildError::Filesystem {
                path: String::from("/"),
            })?;
        let vfs = Vfs::new();
        vfs.mount("/", fat.into_backend(), MountOptions::read_write())
            .await
            .map_err(|_error| ImageBuildError::Filesystem {
                path: String::from("/"),
            })?;
        for entry in entries {
            let path = entry.path();
            match entry {
                SourceEntry::Directory { .. } => vfs.create_dir_all(path).await,
                SourceEntry::File { bytes, .. } => vfs.write(path, bytes).await,
            }
            .map_err(|_error| ImageBuildError::Filesystem {
                path: path.to_owned(),
            })?;
        }
        Ok::<(), ImageBuildError>(())
    })?;
    Arc::try_unwrap(bytes)
        .map_err(|_bytes| ImageBuildError::Filesystem {
            path: String::from("/"),
        })?
        .into_inner()
        .map_err(|_error| ImageBuildError::Filesystem {
            path: String::from("/"),
        })
}

fn build_littlefs_entries(
    capacity: usize,
    entries: &[SourceEntry],
) -> Result<Vec<u8>, ImageBuildError> {
    const BLOCK_SIZE: usize = 4096;

    let block_count = capacity
        .checked_div(BLOCK_SIZE)
        .filter(|count| *count >= 2 && count.saturating_mul(BLOCK_SIZE) == capacity)
        .ok_or(ImageBuildError::InvalidCapacity(capacity))?;

    macro_rules! build_geometry {
        ($blocks:expr) => {{
            build_littlefs_with_geometry::<$blocks>(capacity, entries)
        }};
    }

    match block_count {
        0..2 => Err(ImageBuildError::InvalidCapacity(capacity)),
        2..4 => build_geometry!(2),
        4..8 => build_geometry!(4),
        8..16 => build_geometry!(8),
        16..32 => build_geometry!(16),
        32..64 => build_geometry!(32),
        64..128 => build_geometry!(64),
        128..256 => build_geometry!(128),
        256..384 => build_geometry!(256),
        384..512 => build_geometry!(384),
        512..768 => build_geometry!(512),
        768..1_024 => build_geometry!(768),
        1_024..1_536 => build_geometry!(1_024),
        1_536..2_048 => build_geometry!(1_536),
        2_048..3_072 => build_geometry!(2_048),
        3_072..4_096 => build_geometry!(3_072),
        _ => build_geometry!(4_096),
    }
}

fn build_littlefs_with_geometry<const BLOCK_COUNT: usize>(
    capacity: usize,
    entries: &[SourceEntry],
) -> Result<Vec<u8>, ImageBuildError> {
    let mut storage = ImageStorage::<BLOCK_COUNT>::new(capacity);
    Filesystem::format(&mut storage).map_err(|_error| ImageBuildError::Filesystem {
        path: String::from("/"),
    })?;
    let mut current = String::from("/");
    Filesystem::mount_and_then(&mut storage, |filesystem| {
        for entry in entries {
            let path = entry.path();
            current.clone_from(&path.to_owned());
            let little_path = LittlePathBuf::try_from(path.as_bytes())
                .map_err(|_error| littlefs2::io::Error::INVALID)?;
            match entry {
                SourceEntry::Directory { .. } => filesystem.create_dir_all(&little_path)?,
                SourceEntry::File { bytes, .. } => filesystem.write(&little_path, bytes)?,
            }
        }
        Ok(())
    })
    .map_err(|_error| ImageBuildError::Filesystem { path: current })?;
    Ok(storage.into_bytes())
}

struct ImageStorage<const BLOCK_COUNT: usize> {
    bytes: Vec<u8>,
}

impl<const BLOCK_COUNT: usize> ImageStorage<BLOCK_COUNT> {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn range(&self, offset: usize, length: usize) -> littlefs2::io::Result<Range<usize>> {
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(littlefs2::io::Error::IO)?;
        Ok(offset..end)
    }
}

impl<const BLOCK_COUNT: usize> Storage for ImageStorage<BLOCK_COUNT> {
    const READ_SIZE: usize = 1;
    const WRITE_SIZE: usize = 1;
    const BLOCK_SIZE: usize = 4096;
    const BLOCK_COUNT: usize = BLOCK_COUNT;

    type CACHE_SIZE = U128;
    type LOOKAHEAD_SIZE = U8;

    fn read(&mut self, offset: usize, buffer: &mut [u8]) -> littlefs2::io::Result<usize> {
        let source = self
            .bytes
            .get(self.range(offset, buffer.len())?)
            .ok_or(littlefs2::io::Error::IO)?;
        buffer.copy_from_slice(source);
        Ok(buffer.len())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> littlefs2::io::Result<usize> {
        let range = self.range(offset, bytes.len())?;
        let destination = self.bytes.get_mut(range).ok_or(littlefs2::io::Error::IO)?;
        for (destination, source) in destination.iter_mut().zip(bytes) {
            *destination &= *source;
        }
        Ok(bytes.len())
    }

    fn erase(&mut self, offset: usize, len: usize) -> littlefs2::io::Result<usize> {
        if offset.checked_rem(Self::BLOCK_SIZE) != Some(0)
            || len.checked_rem(Self::BLOCK_SIZE) != Some(0)
        {
            return Err(littlefs2::io::Error::INVALID);
        }
        let range = self.range(offset, len)?;
        self.bytes
            .get_mut(range)
            .ok_or(littlefs2::io::Error::IO)?
            .fill(0xff);
        Ok(len)
    }
}

enum SourceEntry {
    Directory { path: String },
    File { path: String, bytes: Vec<u8> },
}

impl SourceEntry {
    fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path, .. } => path,
        }
    }
}

fn collect_entries(
    root: &Path,
    directory: &Path,
    image_root: &str,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), ImageBuildError> {
    let reader = fs::read_dir(directory).map_err(|source| ImageBuildError::SourceRead {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut children =
        reader
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ImageBuildError::SourceRead {
                path: directory.to_path_buf(),
                source,
            })?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let host = child.path();
        let relative = host
            .strip_prefix(root)
            .map_err(|_error| ImageBuildError::InvalidPath(host.clone()))?;
        let path = image_path(&host, image_root, relative)?;
        let file_type = child
            .file_type()
            .map_err(|source| ImageBuildError::SourceRead {
                path: host.clone(),
                source,
            })?;
        if file_type.is_dir() {
            entries.push(SourceEntry::Directory { path });
            collect_entries(root, &host, image_root, entries)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&host).map_err(|source| ImageBuildError::SourceRead {
                path: host.clone(),
                source,
            })?;
            entries.push(SourceEntry::File { path, bytes });
        } else {
            return Err(ImageBuildError::UnsupportedEntry(host));
        }
    }
    Ok(())
}

fn collect_plugin_resources(
    workspace: &Path,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), ImageBuildError> {
    let plugins_root = workspace.join("plugins");
    match fs::symlink_metadata(&plugins_root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_metadata) => return Err(ImageBuildError::SourceNotDirectory(plugins_root)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ImageBuildError::SourceRead {
                path: plugins_root,
                source,
            });
        }
    }

    let disabled = disabled_plugins(workspace)?;
    let mut ids = BTreeSet::new();
    let mut has_plugins_root = false;
    let mut has_workspace_root = false;
    let mut workspace_paths = BTreeMap::new();
    for plugin in read_directory(&plugins_root)? {
        let file_type = plugin
            .file_type()
            .map_err(|source| ImageBuildError::SourceRead {
                path: plugin.path(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }
        let directory = plugin
            .file_name()
            .into_string()
            .map_err(|_name| ImageBuildError::InvalidPath(plugin.path()))?;
        if disabled.contains(&directory) {
            continue;
        }
        if read_directory(&plugin.path())?.is_empty() {
            continue;
        }

        let manifest_path = plugin.path().join("plugin.toml");
        let manifest_text =
            fs::read_to_string(&manifest_path).map_err(|source| ImageBuildError::SourceRead {
                path: manifest_path.clone(),
                source,
            })?;
        let manifest = parse_plugin_manifest(&manifest_text).map_err(|source| {
            ImageBuildError::PluginManifest {
                path: manifest_path,
                source,
            }
        })?;
        let id = manifest.id();
        if id == "." || id == ".." || id.contains('/') || id.as_bytes().contains(&0) {
            return Err(ImageBuildError::InvalidPath(plugin.path()));
        }
        if !ids.insert(id.to_owned()) {
            return Err(ImageBuildError::DuplicatePluginId(id.to_owned()));
        }
        if !has_plugins_root {
            entries.push(SourceEntry::Directory {
                path: String::from("/plugins"),
            });
            has_plugins_root = true;
        }
        let plugin_image_root = format!("/plugins/{id}");
        entries.push(SourceEntry::Directory {
            path: plugin_image_root.clone(),
        });

        let filesystem_root = plugin.path().join("filesystem");
        match fs::symlink_metadata(&filesystem_root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_metadata) => {
                return Err(ImageBuildError::UnsupportedPluginFilesystemEntry(
                    filesystem_root,
                ));
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(ImageBuildError::SourceRead {
                    path: filesystem_root,
                    source,
                });
            }
        }

        for entry in read_directory(&filesystem_root)? {
            let entry_path = entry.path();
            let is_directory = entry
                .file_type()
                .map_err(|source| ImageBuildError::SourceRead {
                    path: entry_path.clone(),
                    source,
                })?
                .is_dir();
            if entry.file_name() == "resources" && is_directory {
                collect_entries(&entry_path, &entry_path, &plugin_image_root, entries)?;
                continue;
            }
            if entry.file_name() != "workspace" || !is_directory {
                return Err(ImageBuildError::UnsupportedPluginFilesystemEntry(
                    entry_path,
                ));
            }

            for workspace_entry in read_directory(&entry_path)? {
                let workspace_entry_path = workspace_entry.path();
                let is_resource_directory = workspace_entry.file_name() == "resources"
                    && workspace_entry
                        .file_type()
                        .map_err(|source| ImageBuildError::SourceRead {
                            path: workspace_entry_path.clone(),
                            source,
                        })?
                        .is_dir();
                if !is_resource_directory {
                    return Err(ImageBuildError::UnsupportedPluginFilesystemEntry(
                        workspace_entry_path,
                    ));
                }

                if !has_workspace_root {
                    entries.push(SourceEntry::Directory {
                        path: String::from("/workspace"),
                    });
                    has_workspace_root = true;
                }
                let mut contribution = Vec::new();
                collect_entries(
                    &workspace_entry_path,
                    &workspace_entry_path,
                    "/workspace",
                    &mut contribution,
                )?;
                merge_workspace_entries(entries, contribution, &mut workspace_paths)?;
            }
        }
    }
    Ok(())
}

fn merge_workspace_entries(
    entries: &mut Vec<SourceEntry>,
    contribution: Vec<SourceEntry>,
    workspace_paths: &mut BTreeMap<String, bool>,
) -> Result<(), ImageBuildError> {
    for entry in contribution {
        let path = entry.path().to_owned();
        let is_directory = matches!(&entry, SourceEntry::Directory { .. });
        match workspace_paths.get(&path) {
            None => {
                workspace_paths.insert(path, is_directory);
                entries.push(entry);
            }
            Some(true) if is_directory => {}
            Some(_) => return Err(ImageBuildError::DuplicateWorkspaceResource(path)),
        }
    }
    Ok(())
}

fn read_directory(directory: &Path) -> Result<Vec<fs::DirEntry>, ImageBuildError> {
    let reader = fs::read_dir(directory).map_err(|source| ImageBuildError::SourceRead {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut entries =
        reader
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ImageBuildError::SourceRead {
                path: directory.to_path_buf(),
                source,
            })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn disabled_plugins(workspace: &Path) -> Result<BTreeSet<String>, ImageBuildError> {
    let path = workspace.join(".barracuda/disabled-plugins");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(ImageBuildError::SourceRead { path, source }),
    };
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect())
}

fn image_path(host: &Path, root: &str, relative: &Path) -> Result<String, ImageBuildError> {
    let mut path = String::from(root);
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ImageBuildError::InvalidPath(host.to_path_buf()));
        };
        let name = name
            .to_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ImageBuildError::InvalidPath(host.to_path_buf()))?;
        path.push('/');
        path.push_str(name);
    }
    if path.is_empty() {
        Err(ImageBuildError::InvalidPath(host.to_path_buf()))
    } else {
        Ok(path)
    }
}

struct ImageDisk {
    bytes: Arc<Mutex<Vec<u8>>>,
    cursor: usize,
}

impl ImageDisk {
    fn new(bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        Self { bytes, cursor: 0 }
    }
}

impl ErrorType for ImageDisk {
    type Error = embedded_io::ErrorKind;
}

impl Read for ImageDisk {
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let bytes = self
            .bytes
            .lock()
            .map_err(|_error| embedded_io::ErrorKind::Other)?;
        let amount = output.len().min(bytes.len().saturating_sub(self.cursor));
        output[..amount].copy_from_slice(&bytes[self.cursor..self.cursor + amount]);
        self.cursor = self
            .cursor
            .checked_add(amount)
            .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        Ok(amount)
    }
}

impl Write for ImageDisk {
    async fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_error| embedded_io::ErrorKind::Other)?;
        let end = self
            .cursor
            .checked_add(input.len())
            .filter(|end| *end <= bytes.len())
            .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        bytes[self.cursor..end].copy_from_slice(input);
        self.cursor = end;
        Ok(input.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for ImageDisk {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        let capacity = self
            .bytes
            .lock()
            .map_err(|_error| embedded_io::ErrorKind::Other)?
            .len() as i128;
        let next = match position {
            SeekFrom::Start(offset) => Some(i128::from(offset)),
            SeekFrom::End(offset) => capacity.checked_add(i128::from(offset)),
            SeekFrom::Current(offset) => (self.cursor as i128).checked_add(i128::from(offset)),
        }
        .filter(|next| *next >= 0 && *next <= capacity)
        .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        self.cursor =
            usize::try_from(next).map_err(|_error| embedded_io::ErrorKind::InvalidInput)?;
        u64::try_from(self.cursor).map_err(|_error| embedded_io::ErrorKind::InvalidInput)
    }
}

fn read_text(path: &Path, kind: &str) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("failed to read {kind} `{}`: {error}", path.display()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLayoutDocument {
    capacity: usize,
    regions: Vec<FileRegionDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRegionDocument {
    name: String,
    offset: u64,
    size: usize,
    access: FileRegionAccess,
    filesystem: FileRegionFilesystem,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FileRegionAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FileRegionFilesystem {
    Raw,
    #[serde(rename = "fatfs")]
    FatFs,
    #[serde(rename = "littlefs")]
    LittleFs,
}

fn file_layout_resources_region(
    layout: &str,
) -> Result<(u64, usize, usize, ResourcesFilesystem), String> {
    let mut documents = yaml_peg::serde::from_str::<FileLayoutDocument>(layout)
        .map_err(|error| format!("invalid file layout: {error}"))?;
    if documents.len() != 1 {
        return Err(format!(
            "file layout must contain one document, found {}",
            documents.len()
        ));
    }
    let document = documents
        .pop()
        .ok_or_else(|| String::from("file layout is empty"))?;
    let region = document
        .regions
        .into_iter()
        .find(|region| region.name == "resources")
        .ok_or_else(|| String::from("selected Board native layout has no `resources` region"))?;
    if region.access != FileRegionAccess::ReadOnly {
        return Err(String::from(
            "selected Board `resources` region is not read-only",
        ));
    }
    let filesystem = match region.filesystem {
        FileRegionFilesystem::FatFs => ResourcesFilesystem::FatFs,
        FileRegionFilesystem::LittleFs => ResourcesFilesystem::LittleFs,
        FileRegionFilesystem::Raw => {
            return Err(String::from(
                "selected Board `resources` region has no filesystem",
            ))
        }
    };
    let end = usize::try_from(region.offset)
        .ok()
        .and_then(|offset| offset.checked_add(region.size))
        .ok_or_else(|| String::from("selected Board `resources` region range overflows"))?;
    if end > document.capacity {
        return Err(String::from(
            "selected Board `resources` region exceeds file-layout capacity",
        ));
    }
    Ok((region.offset, region.size, document.capacity, filesystem))
}

fn esp_resources_region(layout: &str) -> Result<(u64, usize, ResourcesFilesystem), String> {
    let table = PartitionTable::try_from_str(layout)
        .map_err(|error| format!("invalid ESP partition table: {error}"))?;
    table
        .validate()
        .map_err(|error| format!("invalid ESP partition table: {error}"))?;
    let partition = table
        .partitions()
        .iter()
        .find(|partition| partition.name() == "resources")
        .ok_or_else(|| String::from("selected Board native layout has no `resources` partition"))?;
    let filesystem = match partition.subtype() {
        SubType::Data(DataType::Fat) => ResourcesFilesystem::FatFs,
        SubType::Data(DataType::Littlefs) => ResourcesFilesystem::LittleFs,
        _ => {
            return Err(String::from(
                "selected Board `resources` partition is neither FATFS nor LittleFS",
            ))
        }
    };
    if !partition.flags().contains(Flags::READONLY) {
        return Err(String::from(
            "selected Board `resources` partition is not read-only",
        ));
    }
    Ok((
        u64::from(partition.offset()),
        usize::try_from(partition.size())
            .map_err(|_error| String::from("selected Board `resources` partition is too large"))?,
        filesystem,
    ))
}

fn stm32_resources_region(layout: &str) -> Result<(u64, usize, ResourcesFilesystem), String> {
    let declaration = layout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("RESOURCES "))
        .ok_or_else(|| String::from("selected Board native layout has no `RESOURCES` region"))?;
    let attributes = declaration
        .split_once(')')
        .and_then(|(prefix, _rest)| prefix.split_once('(').map(|(_name, attributes)| attributes))
        .ok_or_else(|| String::from("STM32 `RESOURCES` region has no access attributes"))?;
    if attributes.contains('w') {
        return Err(String::from(
            "selected Board `RESOURCES` region is not read-only",
        ));
    }
    let offset = linker_assignment(declaration, "ORIGIN")?;
    let size = linker_assignment(declaration, "LENGTH")?;
    let filesystem = linker_partition_filesystem(declaration)?;
    Ok((
        offset,
        usize::try_from(size)
            .map_err(|_error| String::from("selected Board `RESOURCES` region is too large"))?,
        filesystem,
    ))
}

fn linker_partition_filesystem(declaration: &str) -> Result<ResourcesFilesystem, String> {
    const PREFIX: &str = "/* filesystem:";

    let declaration = declaration
        .split_once(PREFIX)
        .map(|(_prefix, declaration)| declaration)
        .and_then(|declaration| declaration.strip_suffix("*/"))
        .map(str::trim)
        .ok_or_else(|| String::from("STM32 `RESOURCES` region has no filesystem declaration"))?;
    match declaration {
        "fatfs" => Ok(ResourcesFilesystem::FatFs),
        "littlefs" => Ok(ResourcesFilesystem::LittleFs),
        other => Err(format!(
            "STM32 `RESOURCES` region declares unsupported filesystem `{other}`"
        )),
    }
}

fn linker_assignment(declaration: &str, name: &str) -> Result<u64, String> {
    let assignment = declaration
        .split(',')
        .find_map(|field| {
            let (field_name, value) = field.split_once('=')?;
            (field_name.split_whitespace().last()? == name)
                .then(|| value.split_whitespace().next())
                .flatten()
        })
        .ok_or_else(|| format!("STM32 `RESOURCES` region has no {name} assignment"))?;
    parse_linker_size(assignment)
}

fn parse_linker_size(value: &str) -> Result<u64, String> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'K' | b'k') => (&value[..value.len().saturating_sub(1)], 1024),
        Some(b'M' | b'm') => (&value[..value.len().saturating_sub(1)], 1024 * 1024),
        _ => (value, 1),
    };
    let number = if let Some(hexadecimal) = number.strip_prefix("0x") {
        u64::from_str_radix(hexadecimal, 16)
    } else {
        number.parse()
    }
    .map_err(|_error| format!("invalid STM32 layout value `{value}`"))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("STM32 layout value `{value}` overflows"))
}

fn write_image(output: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    let file_name = output
        .file_name()
        .ok_or_else(|| format!("output path has no file name: `{}`", output.display()))?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(".tmp");
    let temporary = output.with_file_name(temporary_name);
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ignored = fs::remove_file(&temporary);
        return Err(format!(
            "failed to write temporary image `{}`: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, output) {
        let _ignored = fs::remove_file(&temporary);
        return Err(format!(
            "failed to install image at `{}`: {error}",
            output.display()
        ));
    }
    Ok(())
}
