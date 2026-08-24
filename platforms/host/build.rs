//! Bakes Host Platform YAML settings into the runtime crate.

use std::{env, error::Error, fs, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformDocument {
    name: String,
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(rename = "type")]
    type_name: String,
    settings: HostSettings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct HostSettings {
    state_directory: String,
    flash_image: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostLayoutDocument {
    capacity: usize,
    regions: Vec<HostRegionDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRegionDocument {
    name: String,
    offset: u32,
    size: u32,
    access: HostRegionAccess,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HostRegionAccess {
    ReadOnly,
    ReadWrite,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=platform.yml");
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");
    let yaml = fs::read_to_string("platform.yml")?;
    let mut documents = yaml_peg::serde::from_str::<PlatformDocument>(&yaml)?;
    if documents.len() != 1 {
        return Err(format!(
            "platform.yml must contain one document, found {}",
            documents.len()
        )
        .into());
    }
    let platform = documents
        .pop()
        .ok_or("platform.yml did not contain a document")?;
    if platform.name != "host"
        || platform.crate_name != "barracuda_platform_host"
        || platform.type_name != "HostPlatform"
    {
        return Err("platform.yml identity does not match the Host crate".into());
    }
    let mut generated = format!(
        "/// Host settings generated from `platform.yml`.\npub const PLATFORM_SETTINGS: HostSettings = HostSettings::new({:?}, {:?});\n",
        platform.settings.state_directory, platform.settings.flash_image,
    );
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| "local-host".into());
    if board_name.is_empty()
        || !board_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid Board name `{board_name}`").into());
    }
    let layout_path = root
        .join("boards/configs")
        .join(&board_name)
        .join("host-layout.yml");
    println!("cargo:rerun-if-changed={}", layout_path.display());
    let layout_yaml = fs::read_to_string(layout_path)?;
    let mut layouts = yaml_peg::serde::from_str::<HostLayoutDocument>(&layout_yaml)?;
    if layouts.len() != 1 {
        return Err("host-layout.yml must contain exactly one document".into());
    }
    let layout = layouts.pop().ok_or("host-layout.yml is empty")?;
    generated.push_str("\nconst BOARD_HOST_REGIONS: &[HostRegion] = &[\n");
    for region in layout.regions {
        let constructor = match region.access {
            HostRegionAccess::ReadOnly => "read_only",
            HostRegionAccess::ReadWrite => "read_write",
        };
        generated.push_str(&format!(
            "    HostRegion::{constructor}({:?}, {}, {}),\n",
            region.name, region.offset, region.size
        ));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "/// Host-native layout selected from the Board bundle.\npub const BOARD_HOST_LAYOUT: HostLayout = HostLayout::new({}, BOARD_HOST_REGIONS);\n",
        layout.capacity
    ));
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("host_config.rs"), generated)?;
    Ok(())
}
