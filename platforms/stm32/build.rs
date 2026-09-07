//! Installs the selected Board's native linker layout and projects every region.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, SELECTED_BOARD_PATH};

struct NativeRegion<'a> {
    name: &'a str,
    access: &'static str,
    filesystem: &'static str,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_LOG_LEVEL");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let selection_path = root.join(SELECTED_BOARD_PATH);
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let board_name =
        read_selected_board(&root)?.ok_or("no Board selected; run `cargo board select` first")?;
    let bundle = root.join("boards/configs").join(&board_name);
    let board_path = bundle.join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());

    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }
    if board.hardware().chip() != "stm32f429zi" {
        let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
        let mut generated = String::from(
            "/// Resolves the inactive STM32 Platform to an empty partition table.\n\
             pub fn board_partition_table(\n\
                 _flash_base: usize,\n\
             ) -> Result<Stm32PartitionTable<0>, LinkerRegionError> {\n\
                 Ok(Stm32PartitionTable::new([]))\n\
             }\n",
        );
        render_log_level(&mut generated)?;
        fs::write(output.join("stm32_layout.rs"), generated)?;
        return Ok(());
    }

    let linker_path = bundle.join(board.native_layout().artifact());
    println!("cargo:rerun-if-changed={}", linker_path.display());
    let linker = fs::read_to_string(&linker_path)?;
    let regions = native_regions(&linker)?;
    if regions.is_empty() {
        return Err("Board linker layout exports no native flash regions".into());
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::copy(linker_path, output.join("memory.x"))?;
    println!("cargo:rustc-link-search={}", output.display());
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm") {
        println!("cargo:rustc-link-arg-examples=-Tlink.x");
    }

    let mut generated = String::from("unsafe extern \"C\" {\n");
    for region in &regions {
        let symbol = rust_symbol(region.name);
        generated.push_str(&format!(
            "    #[link_name = \"__{}_start\"]\n    static {symbol}_START: u8;\n\
             #[link_name = \"__{}_end\"]\n    static {symbol}_END: u8;\n",
            region.name, region.name,
        ));
    }
    generated.push_str("}\n\n");
    generated.push_str("fn symbol_address(symbol: *const u8) -> usize { symbol.addr() }\n\n");
    generated.push_str(&format!(
        "/// Resolves every Board-native linker region against STM32 flash.\n\
         pub fn board_partition_table(\n\
             flash_base: usize,\n\
         ) -> Result<Stm32PartitionTable<{}>, LinkerRegionError> {{\n\
             Ok(Stm32PartitionTable::new([\n",
        regions.len()
    ));
    for region in &regions {
        let symbol = rust_symbol(region.name);
        generated.push_str(&format!(
            "        Stm32Region::new(\n\
                 {:?},\n\
                 LinkerRegion::try_from_addresses(\n\
                     flash_base,\n\
                     symbol_address(core::ptr::addr_of!({symbol}_START)),\n\
                     symbol_address(core::ptr::addr_of!({symbol}_END)),\n\
                 )?,\n\
                 Stm32RegionAccess::{},\n\
                 barracuda_platform::PartitionFilesystem::{},\n\
             ),\n",
            region.name, region.access, region.filesystem,
        ));
    }
    generated.push_str("    ]))\n}\n");
    render_log_level(&mut generated)?;
    fs::write(output.join("stm32_layout.rs"), generated)?;
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

fn native_regions(linker: &str) -> Result<Vec<NativeRegion<'_>>, Box<dyn Error>> {
    let mut regions = Vec::new();
    for line in linker.lines() {
        let Some((name, memory)) = start_symbol(line) else {
            continue;
        };
        validate_symbol(name)?;
        if !linker.contains(&format!("__{name}_end")) {
            return Err(format!("native region `{name}` has no matching end symbol").into());
        }
        let attributes = memory_attributes(linker, memory)?;
        let access = if attributes.contains('w') {
            "ReadWrite"
        } else {
            "ReadOnly"
        };
        let filesystem = memory_filesystem(linker, memory)?;
        regions.push(NativeRegion {
            name,
            access,
            filesystem,
        });
    }
    Ok(regions)
}

fn start_symbol(line: &str) -> Option<(&str, &str)> {
    let (left, right) = line.trim().split_once('=')?;
    let name = left.trim().strip_prefix("__")?.strip_suffix("_start")?;
    let memory = right.trim().strip_prefix("ORIGIN(")?.split_once(')')?.0;
    Some((name, memory))
}

fn memory_attributes<'a>(linker: &'a str, memory: &str) -> Result<&'a str, Box<dyn Error>> {
    let line = memory_declaration(linker, memory)?;
    let remainder = line
        .strip_prefix(memory)
        .ok_or_else(|| format!("invalid MEMORY declaration for `{memory}`"))?;
    let start = remainder
        .find('(')
        .ok_or_else(|| format!("MEMORY region `{memory}` has no native attributes"))?;
    remainder
        .get(start.saturating_add(1)..)
        .and_then(|value| value.split_once(')'))
        .map(|(attributes, _rest)| attributes)
        .ok_or_else(|| format!("MEMORY region `{memory}` has invalid attributes").into())
}

fn memory_declaration<'a>(linker: &'a str, memory: &str) -> Result<&'a str, Box<dyn Error>> {
    for line in linker.lines().map(str::trim) {
        let Some(remainder) = line.strip_prefix(memory) else {
            continue;
        };
        if !remainder
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace() || character == '(')
        {
            continue;
        }
        return Ok(line);
    }
    Err(format!("native symbol references absent MEMORY region `{memory}`").into())
}

fn memory_filesystem(linker: &str, memory: &str) -> Result<&'static str, Box<dyn Error>> {
    const PREFIX: &str = "/* filesystem:";
    let declaration = memory_declaration(linker, memory)?;
    let value = declaration
        .split_once(PREFIX)
        .and_then(|(_prefix, value)| value.strip_suffix("*/"))
        .map(str::trim)
        .ok_or_else(|| format!("MEMORY region `{memory}` has no filesystem declaration"))?;
    match value {
        "raw" => Ok("Raw"),
        "fatfs" => Ok("FatFs"),
        "littlefs" => Ok("LittleFs"),
        other => {
            Err(format!("MEMORY region `{memory}` has unsupported filesystem `{other}`").into())
        }
    }
}

fn rust_symbol(name: &str) -> String {
    name.to_ascii_uppercase()
}

fn validate_symbol(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(format!("invalid linker region label `{value}`").into());
    }
    Ok(())
}
