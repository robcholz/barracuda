//! Bakes Linux Platform and Board-native file-layout settings into Rust.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, SELECTED_BOARD_PATH};
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
    println!("cargo:rerun-if-env-changed=BARRACUDA_LOG_LEVEL");
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
    let root = manifest.join("../..");
    let selection_path = root.join(SELECTED_BOARD_PATH);
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let board_name =
        read_selected_board(&root)?.ok_or("no Board selected; run `cargo board select` first")?;
    let board_directory = manifest
        .join("../..")
        .join("boards/configs")
        .join(&board_name);
    let board_path = board_directory.join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.hardware().chip() != "linux" {
        render_layout(
            &mut generated,
            FileLayoutDocument {
                capacity: 0,
                regions: Vec::new(),
            },
        );
        render_log_level(&mut generated)?;
        let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
        fs::write(output.join("linux_config.rs"), generated)?;
        return Ok(());
    }
    let layout_path = board_directory.join(board.native_layout().artifact());
    println!("cargo:rerun-if-changed={}", layout_path.display());
    let layout_yaml = fs::read_to_string(layout_path)?;
    let mut layouts = yaml_peg::serde::from_str::<FileLayoutDocument>(&layout_yaml)?;
    let layout = exactly_one(&mut layouts, "file-layout.yml")?;
    render_layout(&mut generated, layout);
    render_log_level(&mut generated)?;
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("linux_config.rs"), generated)?;
    Ok(())
}

fn render_log_level(generated: &mut String) -> Result<(), Box<dyn Error>> {
    let level = env::var("BARRACUDA_LOG_LEVEL").unwrap_or_else(|_| String::from("info"));
    let variant = match level.as_str() {
        "off" => "Off",
        "error" => "Error",
        "warn" => "Warn",
        "info" => "Info",
        "debug" => "Debug",
        "trace" => "Trace",
        _ => {
            return Err(format!(
                "invalid BARRACUDA_LOG_LEVEL `{level}`; expected off, error, warn, info, debug, or trace"
            )
            .into());
        }
    };
    generated.push_str(&format!(
        "\n/// Log level selected at build time.\npub const PLATFORM_LOG_LEVEL: ::log::LevelFilter = ::log::LevelFilter::{variant};\n"
    ));
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
