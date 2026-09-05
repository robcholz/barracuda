//! Platform-specific host-side System partition flashing.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlatformFlash {
    File { platform: String, capacity: usize },
    Esp { chip: String },
    Stm32 { chip: String },
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
            "failed to read System image `{}`: {error}; run `cargo system-image build` first",
            request.image.display()
        )
    })?;
    if bytes.len() != request.size {
        return Err(format!(
            "System image `{}` is {} bytes, but the selected Board has a {}-byte `system` partition; run `cargo system-image build` again",
            request.image.display(),
            bytes.len(),
            request.size
        ));
    }

    match request.platform {
        PlatformFlash::File { platform, capacity } => {
            let destination = host_flash_path(request.workspace, platform)?;
            write_file_partition(
                &destination,
                *capacity,
                request.offset,
                request.size,
                &bytes,
            )?;
            Ok(destination.display().to_string())
        }
        PlatformFlash::Esp { chip } => {
            let command = esp_command(chip, request.offset, request.image);
            run(command)?;
            Ok(String::from("espflash"))
        }
        PlatformFlash::Stm32 { chip } => {
            let command = stm32_command(chip, request.offset, request.image);
            run(command)?;
            Ok(String::from("probe-rs"))
        }
    }
}

#[derive(Deserialize)]
struct HostPlatformDocument {
    name: String,
    settings: HostPlatformSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct HostPlatformSettings {
    state_directory: PathBuf,
    flash_image: PathBuf,
}

fn host_flash_path(workspace: &Path, platform: &str) -> Result<PathBuf, String> {
    let definition = workspace
        .join("platforms")
        .join(platform)
        .join("platform.yml");
    let yaml = fs::read_to_string(&definition).map_err(|error| {
        format!(
            "failed to read selected Board Platform definition `{}`: {error}",
            definition.display()
        )
    })?;
    let mut documents =
        yaml_peg::serde::from_str::<HostPlatformDocument>(&yaml).map_err(|error| {
            format!(
                "invalid Platform definition `{}`: {error}",
                definition.display()
            )
        })?;
    if documents.len() != 1 {
        return Err(format!(
            "Platform definition `{}` must contain one document, found {}",
            definition.display(),
            documents.len()
        ));
    }
    let document = documents
        .pop()
        .ok_or_else(|| format!("Platform definition `{}` is empty", definition.display()))?;
    if document.name != platform {
        return Err(format!(
            "Platform directory `{platform}` declares Platform `{}`",
            document.name
        ));
    }
    validate_relative_path(&document.settings.state_directory, "state-directory")?;
    validate_relative_path(&document.settings.flash_image, "flash-image")?;
    Ok(workspace
        .join(document.settings.state_directory)
        .join(document.settings.flash_image))
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

struct FlashCommand {
    program: &'static str,
    arguments: Vec<OsString>,
}

fn esp_command(chip: &str, offset: u64, image: &Path) -> FlashCommand {
    FlashCommand {
        program: "espflash",
        arguments: vec![
            OsString::from("write-bin"),
            OsString::from("--chip"),
            OsString::from(chip),
            OsString::from(format!("{offset:#x}")),
            image.as_os_str().to_owned(),
        ],
    }
}

fn stm32_command(chip: &str, offset: u64, image: &Path) -> FlashCommand {
    FlashCommand {
        program: "probe-rs",
        arguments: vec![
            OsString::from("download"),
            OsString::from("--chip"),
            OsString::from(chip),
            OsString::from("--binary-format"),
            OsString::from("bin"),
            OsString::from("--base-address"),
            OsString::from(format!("{offset:#x}")),
            image.as_os_str().to_owned(),
        ],
    }
}

fn run(command: FlashCommand) -> Result<(), String> {
    let rendered = render_command(&command);
    let status = Command::new(command.program)
        .args(&command.arguments)
        .status()
        .map_err(|error| format!("failed to run `{rendered}`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{rendered}` exited with {status}"))
    }
}

fn render_command(command: &FlashCommand) -> String {
    std::iter::once(OsString::from(command.program))
        .chain(command.arguments.iter().cloned())
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn esp_platform_uses_the_selected_board_chip_and_partition_offset() {
        let command = super::esp_command("esp32c6", 0x52_0000, Path::new("system.img"));

        assert_eq!(command.program, "espflash");
        assert_eq!(
            command.arguments,
            ["write-bin", "--chip", "esp32c6", "0x520000", "system.img"].map(OsString::from)
        );
    }

    #[test]
    fn stm32_platform_uses_the_selected_board_chip_and_absolute_address() {
        let command = super::stm32_command("stm32f429zi", 0x0812_0000, Path::new("system.img"));

        assert_eq!(command.program, "probe-rs");
        assert_eq!(
            command.arguments,
            [
                "download",
                "--chip",
                "stm32f429zi",
                "--binary-format",
                "bin",
                "--base-address",
                "0x8120000",
                "system.img",
            ]
            .map(OsString::from)
        );
    }
}
