//! Host-side construction of Barracuda System partition images.

use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use barracuda_board_config::{parse, read_selected_board};
use esp_idf_part::{Flags, PartitionTable};
use generic_array::typenum::{U128, U8};
use littlefs2::driver::Storage;
use littlefs2::fs::Filesystem;
use littlefs2::path::PathBuf as LittlePathBuf;
use serde::Deserialize;

/// Workspace-relative source tree burned into the System partition.
pub const IMAGE_SOURCE: &str = "image";
/// Workspace-relative output path for the raw System partition image.
pub const IMAGE_OUTPUT: &str = "target/barracuda-system.img";

const IMAGE_BLOCK_SIZE: usize = 4096;

/// Selected Board region assigned to the System filesystem.
#[derive(Debug, PartialEq, Eq)]
pub struct SystemRegion {
    board: String,
    offset: u64,
    size: usize,
}

impl SystemRegion {
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

    /// Returns the System region capacity in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }
}

/// Description of one completed System image build.
#[derive(Debug, PartialEq, Eq)]
pub struct BuiltImage {
    board: String,
    source: PathBuf,
    output: PathBuf,
    offset: u64,
    size: usize,
}

impl BuiltImage {
    /// Returns the selected Board name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the source directory used for the image.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Returns the raw image output path.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// Returns the selected Board's native System-region offset or address.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the raw image size in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }
}

/// Failure while constructing a LittleFS partition image.
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
    /// A source path cannot be represented by the LittleFS path contract.
    InvalidPath(PathBuf),
    /// The image capacity is too small or is not erase-block aligned.
    InvalidCapacity(usize),
    /// LittleFS could not format or populate the image.
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
                write!(formatter, "image source is not a directory: `{}`", path.display())
            }
            Self::UnsupportedEntry(path) => write!(
                formatter,
                "image source contains an unsupported entry: `{}`",
                path.display()
            ),
            Self::InvalidPath(path) => write!(
                formatter,
                "image path cannot be represented in LittleFS: `{}`",
                path.display()
            ),
            Self::InvalidCapacity(capacity) => write!(
                formatter,
                "image capacity {capacity} must contain at least two {IMAGE_BLOCK_SIZE}-byte erase blocks"
            ),
            Self::Filesystem { path } => {
                write!(formatter, "failed to populate LittleFS path `{path}`")
            }
        }
    }
}

impl std::error::Error for ImageBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRead { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Builds the top-level [`IMAGE_SOURCE`] tree for the currently selected Board.
///
/// # Errors
///
/// Returns an error when Board selection or its native layout is invalid, the
/// source tree cannot be converted to LittleFS, or the output cannot be written.
pub fn build_selected(workspace: &Path) -> Result<BuiltImage, String> {
    let region = selected_system_region(workspace)?;
    let source = workspace.join(IMAGE_SOURCE);
    let output = workspace.join(IMAGE_OUTPUT);
    let bytes = build_directory(&source, region.size).map_err(|error| error.to_string())?;
    write_image(&output, &bytes)?;
    Ok(BuiltImage {
        board: region.board,
        source,
        output,
        offset: region.offset,
        size: region.size,
    })
}

/// Resolves the System region from the currently selected Board's native layout.
///
/// # Errors
///
/// Returns an error when no Board is selected, the selected bundle is invalid,
/// or its native layout does not contain a writable System region.
pub fn selected_system_region(workspace: &Path) -> Result<SystemRegion, String> {
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
    let (offset, size) = match board.hardware().chip() {
        "macos" | "linux" => file_layout_system_region(&layout)?,
        chip if chip.starts_with("esp32") => esp_system_region(&layout)?,
        chip if chip.starts_with("stm32") => stm32_system_region(&layout)?,
        chip => return Err(format!("unsupported native layout for Board chip `{chip}`")),
    };
    Ok(SystemRegion {
        board: board_name,
        offset,
        size,
    })
}

/// Builds one raw LittleFS image from all files below `source`.
///
/// Entries are traversed in lexical order, so identical source trees produce
/// identical images. Symbolic links and special entries are rejected.
///
/// # Errors
///
/// Returns [`ImageBuildError`] when the source is invalid, the capacity is not
/// supported, or LittleFS cannot format or populate the image.
pub fn build_directory(source: &Path, capacity: usize) -> Result<Vec<u8>, ImageBuildError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|source_error| ImageBuildError::SourceRead {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    if !metadata.is_dir() {
        return Err(ImageBuildError::SourceNotDirectory(source.to_path_buf()));
    }
    let block_count = capacity
        .checked_div(IMAGE_BLOCK_SIZE)
        .filter(|count| *count >= 2 && count.saturating_mul(IMAGE_BLOCK_SIZE) == capacity)
        .ok_or(ImageBuildError::InvalidCapacity(capacity))?;
    let mut entries = Vec::new();
    collect_entries(source, source, &mut entries)?;

    macro_rules! build_geometry {
        ($blocks:expr) => {{
            build_with_geometry::<$blocks>(capacity, &entries)
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

enum SourceEntry {
    Directory { path: String },
    File { path: String, bytes: Vec<u8> },
}

fn collect_entries(
    root: &Path,
    directory: &Path,
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
        let path = image_path(&host, relative)?;
        let file_type = child
            .file_type()
            .map_err(|source| ImageBuildError::SourceRead {
                path: host.clone(),
                source,
            })?;
        if file_type.is_dir() {
            entries.push(SourceEntry::Directory { path });
            collect_entries(root, &host, entries)?;
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

fn build_with_geometry<const BLOCK_COUNT: usize>(
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
            let path = match entry {
                SourceEntry::Directory { path } | SourceEntry::File { path, .. } => path,
            };
            current.clone_from(path);
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

fn image_path(host: &Path, relative: &Path) -> Result<String, ImageBuildError> {
    let mut path = String::new();
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
    const BLOCK_SIZE: usize = IMAGE_BLOCK_SIZE;
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
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FileRegionAccess {
    ReadOnly,
    ReadWrite,
}

fn file_layout_system_region(layout: &str) -> Result<(u64, usize), String> {
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
        .find(|region| region.name == "system")
        .ok_or_else(|| String::from("selected Board native layout has no `system` region"))?;
    if region.access != FileRegionAccess::ReadWrite {
        return Err(String::from("selected Board `system` region is read-only"));
    }
    let end = usize::try_from(region.offset)
        .ok()
        .and_then(|offset| offset.checked_add(region.size))
        .ok_or_else(|| String::from("selected Board `system` region range overflows"))?;
    if end > document.capacity {
        return Err(String::from(
            "selected Board `system` region exceeds file-layout capacity",
        ));
    }
    Ok((region.offset, region.size))
}

fn esp_system_region(layout: &str) -> Result<(u64, usize), String> {
    let table = PartitionTable::try_from_str(layout)
        .map_err(|error| format!("invalid ESP partition table: {error}"))?;
    table
        .validate()
        .map_err(|error| format!("invalid ESP partition table: {error}"))?;
    let partition = table
        .partitions()
        .iter()
        .find(|partition| partition.name() == "system")
        .ok_or_else(|| String::from("selected Board native layout has no `system` partition"))?;
    if partition.flags().contains(Flags::READONLY) {
        return Err(String::from(
            "selected Board `system` partition is read-only",
        ));
    }
    Ok((
        u64::from(partition.offset()),
        usize::try_from(partition.size())
            .map_err(|_error| String::from("selected Board `system` partition is too large"))?,
    ))
}

fn stm32_system_region(layout: &str) -> Result<(u64, usize), String> {
    let declaration = layout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("SYSTEM "))
        .ok_or_else(|| String::from("selected Board native layout has no `SYSTEM` region"))?;
    let offset = linker_assignment(declaration, "ORIGIN")?;
    let size = linker_assignment(declaration, "LENGTH")?;
    Ok((
        offset,
        usize::try_from(size)
            .map_err(|_error| String::from("selected Board `SYSTEM` region is too large"))?,
    ))
}

fn linker_assignment(declaration: &str, name: &str) -> Result<u64, String> {
    let assignment = declaration
        .split(',')
        .find_map(|field| {
            let (field_name, value) = field.split_once('=')?;
            (field_name.split_whitespace().last()? == name).then_some(value.trim())
        })
        .ok_or_else(|| format!("STM32 `SYSTEM` region has no {name} assignment"))?;
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
