//! Platform-specific host-side System partition flashing.

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use barracuda_platform_config::CommandDriver;

use crate::command::{self, DriverContext};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlatformFlash {
    File {
        state_directory: PathBuf,
        flash_image: PathBuf,
        capacity: usize,
    },
    Command {
        platform_directory: PathBuf,
        layout: PathBuf,
        chip: String,
        driver: CommandDriver,
    },
}

pub(crate) struct FlashRequest<'a> {
    pub(crate) workspace: &'a Path,
    pub(crate) image: &'a Path,
    pub(crate) offset: u64,
    pub(crate) size: usize,
    pub(crate) platform: &'a PlatformFlash,
}

pub(crate) fn flash(request: FlashRequest<'_>) -> Result<String, String> {
    let bytes = fs::read(request.image).map_err(|error| {
        format!(
            "failed to read System image `{}`: {error}; run `cargo image build` first",
            request.image.display()
        )
    })?;
    if bytes.len() != request.size {
        return Err(format!(
            "System image `{}` is {} bytes, but the selected Board has a {}-byte `resources` partition; run `cargo image build` again",
            request.image.display(),
            bytes.len(),
            request.size
        ));
    }

    match request.platform {
        PlatformFlash::File {
            state_directory,
            flash_image,
            capacity,
        } => {
            let destination = host_flash_path(request.workspace, state_directory, flash_image)?;
            write_file_partition(
                &destination,
                *capacity,
                request.offset,
                request.size,
                &bytes,
            )?;
            Ok(destination.display().to_string())
        }
        PlatformFlash::Command {
            platform_directory,
            layout,
            chip,
            driver,
        } => {
            let command = command::prepare(
                driver,
                DriverContext {
                    workspace: request.workspace,
                    platform: platform_directory,
                    layout,
                    chip,
                    image: Some(request.image),
                    offset: Some(request.offset),
                    size: Some(request.size),
                },
            );
            let destination = command.program.display().to_string();
            command::status(&command)?;
            Ok(destination)
        }
    }
}

fn host_flash_path(
    workspace: &Path,
    state_directory: &Path,
    flash_image: &Path,
) -> Result<PathBuf, String> {
    validate_relative_path(state_directory, "state-directory")?;
    validate_relative_path(flash_image, "flash-image")?;
    Ok(workspace.join(state_directory).join(flash_image))
}

fn validate_relative_path(path: &Path, field: &str) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "Platform `{field}` must be a workspace-relative path without parent traversal: `{}`",
            path.display()
        ));
    }
    Ok(())
}

fn write_file_partition(
    destination: &Path,
    capacity: usize,
    offset: u64,
    partition_size: usize,
    bytes: &[u8],
) -> Result<(), String> {
    usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_add(partition_size))
        .filter(|end| *end <= capacity)
        .ok_or_else(|| {
            format!(
                "selected Board `system` partition at {offset:#x} with size {partition_size} exceeds {capacity}-byte Platform flash"
            )
        })?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create Platform state directory `{}`: {error}",
            parent.display()
        )
    })?;
    let mut flash = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(destination)
        .map_err(|error| {
            format!(
                "failed to open Platform flash `{}`: {error}",
                destination.display()
            )
        })?;
    let actual = usize::try_from(
        flash
            .metadata()
            .map_err(|error| {
                format!(
                    "failed to inspect Platform flash `{}`: {error}",
                    destination.display()
                )
            })?
            .len(),
    )
    .map_err(|_error| format!("Platform flash `{}` is too large", destination.display()))?;
    if actual == 0 {
        initialize_erased(&mut flash, capacity, destination)?;
    } else if actual != capacity {
        return Err(format!(
            "Platform flash `{}` is {actual} bytes, expected {capacity}",
            destination.display()
        ));
    }
    flash.seek(SeekFrom::Start(offset)).map_err(|error| {
        format!(
            "failed to seek Platform flash `{}`: {error}",
            destination.display()
        )
    })?;
    flash.write_all(bytes).map_err(|error| {
        format!(
            "failed to write Platform flash `{}`: {error}",
            destination.display()
        )
    })?;
    flash.sync_all().map_err(|error| {
        format!(
            "failed to synchronize Platform flash `{}`: {error}",
            destination.display()
        )
    })
}

fn initialize_erased(
    flash: &mut fs::File,
    capacity: usize,
    destination: &Path,
) -> Result<(), String> {
    const ERASED: [u8; 4096] = [0xff; 4096];
    let complete_chunks = capacity / ERASED.len();
    let remainder = capacity % ERASED.len();
    for _ in 0..complete_chunks {
        flash.write_all(&ERASED).map_err(|error| {
            format!(
                "failed to initialize Platform flash `{}`: {error}",
                destination.display()
            )
        })?;
    }
    flash.write_all(&ERASED[..remainder]).map_err(|error| {
        format!(
            "failed to initialize Platform flash `{}`: {error}",
            destination.display()
        )
    })
}
