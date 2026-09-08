//! Bakes the selected Browser Board layout and links WASI C runtime support.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, SELECTED_BOARD_PATH};
use serde::Deserialize;

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
    filesystem: FileRegionFilesystem,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FileRegionAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FileRegionFilesystem {
    Raw,
    Fatfs,
    Littlefs,
}

fn main() -> Result<(), Box<dyn Error>> {
    generate_board_layout()?;
    link_wasi_support()?;
    Ok(())
}

fn generate_board_layout() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let selection_path = root.join(SELECTED_BOARD_PATH);
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let board_name =
        read_selected_board(&root)?.ok_or("no Board selected; run `cargo board select` first")?;
    let board_directory = root.join("boards/configs").join(board_name);
    let board_path = board_directory.join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());
    let board = parse(&fs::read_to_string(board_path)?)?;

    let mut generated = String::new();
    if board.hardware().chip() == "browser" {
        let layout_path = board_directory.join(board.native_layout().artifact());
        println!("cargo:rerun-if-changed={}", layout_path.display());
        let mut layouts =
            yaml_peg::serde::from_str::<FileLayoutDocument>(&fs::read_to_string(layout_path)?)?;
        let layout = exactly_one(&mut layouts, "file-layout.yml")?;
        render_layout(&mut generated, layout);
    } else {
        render_inactive_layout(&mut generated);
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("browser_layout.rs"), generated)?;
    Ok(())
}

fn link_wasi_support() -> Result<(), Box<dyn Error>> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("wasi") {
        return Ok(());
    }

    println!("cargo::rerun-if-env-changed=WASI_SDK_PATH");
    let sdk = PathBuf::from(
        env::var_os("WASI_SDK_PATH")
            .ok_or("Browser builds require WASI_SDK_PATH to point to an installed WASI SDK")?,
    );
    let root = sdk.join("share/wasi-sysroot/lib/wasm32-wasip1");
    println!("cargo::rustc-link-search=native={}", root.display());
    println!("cargo::rustc-link-lib=static=wasi-emulated-signal");
    println!("cargo::rustc-link-lib=static=setjmp");
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

fn render_inactive_layout(generated: &mut String) {
    generated.push_str(
        "const BOARD_FILE_REGIONS: &[FileRegion] = &[];\n\
         const BOARD_FILE_LAYOUT: FileLayout = FileLayout::new(0, BOARD_FILE_REGIONS);\n",
    );
}

fn render_layout(generated: &mut String, layout: FileLayoutDocument) {
    generated.push_str("const BOARD_FILE_REGIONS: &[FileRegion] = &[\n");
    for region in layout.regions {
        let access = match region.access {
            FileRegionAccess::ReadOnly => "ReadOnly",
            FileRegionAccess::ReadWrite => "ReadWrite",
        };
        let filesystem = match region.filesystem {
            FileRegionFilesystem::Raw => "Raw",
            FileRegionFilesystem::Fatfs => "FatFs",
            FileRegionFilesystem::Littlefs => "LittleFs",
        };
        generated.push_str(&format!(
            "    FileRegion::new({:?}, {}, {}, FileRegionAccess::{access}, barracuda_platform::PartitionFilesystem::{filesystem}),\n",
            region.name, region.offset, region.size
        ));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "const BOARD_FILE_LAYOUT: FileLayout = FileLayout::new({}, BOARD_FILE_REGIONS);\n",
        layout.capacity
    ));
}
