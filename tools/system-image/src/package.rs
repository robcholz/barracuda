//! Release packaging: the selected Board's firmware, resource image, and the
//! manifest that says where each part is flashed.

use std::fs;
use std::path::{Path, PathBuf};

use barracuda_platform_config::{LayoutDriver, ReleaseDriver};
use esp_idf_part::PartitionTable;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::command::{self, DriverContext, ReleaseContext};
use crate::{build_workspace, resources_region, write_image, Selection};

/// Workspace-relative directory that holds one folder per packaged Board.
pub const PACKAGE_OUTPUT: &str = "target/package";
/// Workspace-relative directory that holds one firmware folder per Board.
pub const FIRMWARE_OUTPUT: &str = "target/firmware";
/// The application binary `cargo build --release` produces.
const APPLICATION: &str = "barracuda-system";
const RESOURCES: &str = "resources.img";
const MANIFEST: &str = "manifest.json";
/// Flash sizes a bootloader header can name, in MiB.
const FLASH_SIZES: [u64; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

/// One packaged Board release.
#[derive(Debug, PartialEq, Eq)]
pub struct Package {
    board: String,
    version: String,
    directory: PathBuf,
    parts: Vec<PackagePart>,
}

impl Package {
    /// Returns the packaged Board's name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the release version the package carries.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the folder holding the parts and `manifest.json`.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the files a flash tool writes, in flashing order.
    #[must_use]
    pub fn parts(&self) -> &[PackagePart] {
        &self.parts
    }
}

/// One Board's release firmware, written without a resource image.
#[derive(Debug, PartialEq, Eq)]
pub struct Firmware {
    board: String,
    path: PathBuf,
    offset: Option<u64>,
    size: u64,
}

impl Firmware {
    /// Returns the Board's name.
    #[must_use]
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Returns the firmware file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the flash offset of a raw image; `None` for an ELF.
    #[must_use]
    pub const fn offset(&self) -> Option<u64> {
        self.offset
    }

    /// Returns the file's size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// One file of a package and where it goes.
#[derive(Debug, PartialEq, Eq)]
pub struct PackagePart {
    path: String,
    offset: Option<u64>,
    size: usize,
}

impl PackagePart {
    /// Returns the file's name inside the package.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the flash offset or address of a raw image; `None` for an ELF,
    /// which carries its own load addresses.
    #[must_use]
    pub const fn offset(&self) -> Option<u64> {
        self.offset
    }

    /// Returns the file's size in bytes.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }
}

#[derive(Serialize)]
struct Manifest<'a> {
    version: &'a str,
    board: &'a str,
    chip: &'a str,
    platform: &'a str,
    parts: Vec<ManifestPart<'a>>,
}

#[derive(Serialize)]
struct ManifestPart<'a> {
    path: &'a str,
    format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
    size: usize,
    sha256: String,
}

/// Packages the selected Board's release build into
/// `target/package/barracuda-<version>-<board>/`.
///
/// It reads the application `cargo build --release` built, turns it into the
/// file the Platform's `application.release` driver names, builds the
/// resource image, and writes `manifest.json` beside them. `version` is the
/// one the build was given in `BARRACUDA_VERSION`.
///
/// # Errors
///
/// Returns an error when the selection or layout is invalid, the Platform
/// publishes no firmware, the release build is missing, or a part cannot be
/// produced or written.
pub fn package_selected(workspace: &Path, version: &str) -> Result<Package, String> {
    let selection = Selection::read(workspace)?;
    let platform = &selection.platform;
    let region = resources_region(workspace, &selection)?;
    let directory = workspace
        .join(PACKAGE_OUTPUT)
        .join(format!("barracuda-{version}-{}", selection.board_name));
    let firmware = write_firmware(workspace, &selection, &directory)?;
    let resources = build_workspace(workspace, region.size(), region.filesystem())
        .map_err(|error| error.to_string())?;
    write_image(&directory.join(RESOURCES), &resources)?;

    let mut parts = Vec::new();
    let mut manifest_parts = Vec::new();
    for (path, offset) in [firmware, (RESOURCES, Some(region.offset()))] {
        let bytes = fs::read(directory.join(path))
            .map_err(|error| format!("failed to read `{path}`: {error}"))?;
        manifest_parts.push(ManifestPart {
            path,
            format: if offset.is_some() { "raw" } else { "elf" },
            offset,
            size: bytes.len(),
            sha256: sha256(&bytes),
        });
        parts.push(PackagePart {
            path: path.to_owned(),
            offset,
            size: bytes.len(),
        });
    }
    let manifest = Manifest {
        version,
        board: &selection.board_name,
        chip: selection.chip(),
        platform: platform.name(),
        parts: manifest_parts,
    };
    let mut json = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("failed to encode {MANIFEST}: {error}"))?;
    json.push(b'\n');
    write_image(&directory.join(MANIFEST), &json)?;

    Ok(Package {
        board: selection.board_name,
        version: version.to_owned(),
        directory,
        parts,
    })
}

/// Turns the selected Board's release build into its release firmware in
/// `target/firmware/<board>/`, as [`package_selected`] does, without the
/// resource image.
///
/// The Platform's release driver writes the file, and the ESP driver
/// refuses an application larger than the Board's first app slot, so this
/// checks that the firmware fits.
///
/// # Errors
///
/// Returns an error when the selection is invalid, the Platform publishes no
/// firmware, the release build is missing, or the release driver fails.
pub fn firmware_selected(workspace: &Path) -> Result<Firmware, String> {
    let selection = Selection::read(workspace)?;
    let directory = workspace.join(FIRMWARE_OUTPUT).join(&selection.board_name);
    let (path, offset) = write_firmware(workspace, &selection, &directory)?;
    let path = directory.join(path);
    let size = fs::metadata(&path)
        .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?
        .len();
    Ok(Firmware {
        board: selection.board_name,
        path,
        offset,
        size,
    })
}

/// Writes the selected Board's release firmware into a fresh `directory` and
/// returns its file name and flash offset (`None` for an ELF).
fn write_firmware(
    workspace: &Path,
    selection: &Selection,
    directory: &Path,
) -> Result<(&'static str, Option<u64>), String> {
    let platform = &selection.platform;
    let release = platform.application().release().ok_or_else(|| {
        format!(
            "Platform `{}` publishes no release firmware",
            platform.name()
        )
    })?;
    let target = selection
        .board
        .toolchain()
        .map(|toolchain| toolchain.target())
        .ok_or_else(|| {
            format!(
                "Board `{}` names no Rust target to release",
                selection.board_name
            )
        })?;
    let application = workspace
        .join("target")
        .join(target)
        .join("release")
        .join(APPLICATION);
    if !application.is_file() {
        return Err(format!(
            "no release build at `{}`; run `cargo build --release` first",
            application.display()
        ));
    }
    if directory.exists() {
        fs::remove_dir_all(directory)
            .map_err(|error| format!("failed to clear `{}`: {error}", directory.display()))?;
    }
    fs::create_dir_all(directory)
        .map_err(|error| format!("failed to create `{}`: {error}", directory.display()))?;

    match release {
        ReleaseDriver::Elf => {
            let path = "firmware.elf";
            fs::copy(&application, directory.join(path))
                .map_err(|error| format!("failed to copy `{}`: {error}", application.display()))?;
            Ok((path, None))
        }
        ReleaseDriver::Command(release) => {
            let path = "firmware.bin";
            let output = directory.join(path);
            let flash_size = flash_size(selection)?;
            let command = command::prepare_release(
                release,
                DriverContext {
                    workspace,
                    platform: platform.directory(),
                    layout: &selection.layout_path,
                    chip: selection.chip(),
                    image: None,
                    offset: None,
                    size: None,
                    release: Some(ReleaseContext {
                        application: &application,
                        output: &output,
                        flash_size: flash_size.as_deref(),
                    }),
                },
            )?;
            command::status(&command)?;
            if !output.is_file() {
                return Err(format!(
                    "`{}` wrote no `{}`",
                    command::render(&command),
                    output.display()
                ));
            }
            Ok((path, Some(release.offset())))
        }
    }
}

/// The flash size a release image's header names: the Board's declared size,
/// or else the smallest that holds its ESP partition table.
fn flash_size(selection: &Selection) -> Result<Option<String>, String> {
    if let Some(declared) = selection.board.hardware().flash_size() {
        return Ok(Some(declared.to_owned()));
    }
    if !matches!(
        selection.platform.system_image().layout(),
        LayoutDriver::EspIdfPartitions
    ) {
        return Ok(None);
    }
    let table = PartitionTable::try_from_str(&selection.layout)
        .map_err(|error| format!("invalid ESP partition table: {error}"))?;
    let end = table
        .partitions()
        .iter()
        .map(|partition| u64::from(partition.offset()) + u64::from(partition.size()))
        .max()
        .unwrap_or(0);
    Ok(smallest_flash_size(end))
}

fn smallest_flash_size(end: u64) -> Option<String> {
    FLASH_SIZES
        .into_iter()
        .find(|mebibytes| mebibytes * 1024 * 1024 >= end)
        .map(|mebibytes| format!("{mebibytes}mb"))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{sha256, smallest_flash_size};

    #[test]
    fn the_flash_size_is_the_smallest_that_holds_the_layout() {
        assert_eq!(smallest_flash_size(0x40_0000).as_deref(), Some("4mb"));
        assert_eq!(smallest_flash_size(0x40_0001).as_deref(), Some("8mb"));
        assert_eq!(smallest_flash_size(0x94_0000).as_deref(), Some("16mb"));
        assert_eq!(smallest_flash_size(0x1_0000_0000), None);
    }

    #[test]
    fn digests_are_lowercase_hex() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
