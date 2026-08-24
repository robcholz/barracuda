//! Bakes Linux Platform and Board-native file-layout settings into Rust.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::parse;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformDocument {
    name: String,
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(rename = "type")]
    type_name: String,
    settings: PlatformSettings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct PlatformSettings {
    state_directory: String,
    flash_image: String,
    network_interface: String,
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
    offset: u32,
    size: u32,
    access: FileRegionAccess,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FileRegionAccess {
    ReadOnly,
    ReadWrite,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=platform.yml");
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");
    let yaml = fs::read_to_string("platform.yml")?;
    let mut documents = yaml_peg::serde::from_str::<PlatformDocument>(&yaml)?;
    let platform = exactly_one(&mut documents, "platform.yml")?;
    if platform.name != "linux"
        || platform.crate_name != "barracuda_platform_linux"
        || platform.type_name != "LinuxPlatform"
    {
        return Err("platform.yml identity does not match the Linux crate".into());
    }

    let mut generated = format!(
        "/// Linux settings generated from `platform.yml`.\npub const PLATFORM_SETTINGS: LinuxSettings = LinuxSettings::new({:?}, {:?}, {:?});\n",
        platform.settings.state_directory,
        platform.settings.flash_image,
        platform.settings.network_interface,
    );
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| "local-linux".into());
    validate_name(&board_name)?;
    let board_directory = manifest
        .join("../..")
        .join("boards/configs")
        .join(&board_name);
    let board_path = board_directory.join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.hardware().chip() != "linux" {
        return Err(format!(
            "Board `{board_name}` chip `{}` is incompatible with the Linux Platform",
            board.hardware().chip()
        )
        .into());
    }
    let layout_path = board_directory.join(board.native_layout().artifact());
    println!("cargo:rerun-if-changed={}", layout_path.display());
    let layout_yaml = fs::read_to_string(layout_path)?;
    let mut layouts = yaml_peg::serde::from_str::<FileLayoutDocument>(&layout_yaml)?;
    let layout = exactly_one(&mut layouts, "file-layout.yml")?;
    render_layout(&mut generated, layout);
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("linux_config.rs"), generated)?;
    Ok(())
}

fn exactly_one<T>(documents: &mut Vec<T>, source: &str) -> Result<T, Box<dyn Error>> {
    if documents.len() != 1 {
        return Err(format!("{source} must contain exactly one document").into());
    }
    documents
        .pop()
        .ok_or_else(|| format!("{source} is empty").into())
}

fn validate_name(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid Board name `{value}`").into());
    }
    Ok(())
}

fn render_layout(generated: &mut String, layout: FileLayoutDocument) {
    generated.push_str("\nconst BOARD_FILE_REGIONS: &[FileRegion] = &[\n");
    for region in layout.regions {
        let constructor = match region.access {
            FileRegionAccess::ReadOnly => "read_only",
            FileRegionAccess::ReadWrite => "read_write",
        };
        generated.push_str(&format!(
            "    FileRegion::{constructor}({:?}, {}, {}),\n",
            region.name, region.offset, region.size
        ));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "/// File-backed layout selected from the Board bundle.\npub const BOARD_FILE_LAYOUT: FileLayout = FileLayout::new({}, BOARD_FILE_REGIONS);\n",
        layout.capacity
    ));
}
