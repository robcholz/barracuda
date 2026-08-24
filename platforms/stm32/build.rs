//! Installs the selected Board's native linker layout and binds storage symbols.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::parse;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| "stm32f429zi-nucleo".into());
    validate_name(&board_name)?;
    let bundle = root.join("boards/configs").join(&board_name);
    let board_path = bundle.join("board.yml");
    let linker_path = bundle.join("memory.x");
    println!("cargo:rerun-if-changed={}", board_path.display());
    println!("cargo:rerun-if-changed={}", linker_path.display());

    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }
    if board.hardware().chip() != "stm32f429zi" {
        return Err(format!(
            "STM32 example currently enables stm32f429zi, not `{}`",
            board.hardware().chip()
        )
        .into());
    }

    let linker = fs::read_to_string(&linker_path)?;
    let filesystem = symbol_pair(&linker, board.storage().filesystem(), "filesystem")?;
    let database = symbol_pair(&linker, board.storage().database(), "database")?;
    let web_assets = board
        .storage()
        .web_assets()
        .map(|name| symbol_pair(&linker, name, "web-assets"))
        .transpose()?;

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::copy(linker_path, output.join("memory.x"))?;
    println!("cargo:rustc-link-search={}", output.display());
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm") {
        println!("cargo:rustc-link-arg-examples=-Tlink.x");
    }

    let mut generated = String::new();
    generated.push_str("unsafe extern \"C\" {\n");
    generated.push_str(&render_symbols("FILESYSTEM", filesystem));
    generated.push_str(&render_symbols("DATABASE", database));
    if let Some(symbols) = web_assets {
        generated.push_str(&render_symbols("WEB_ASSETS", symbols));
    }
    generated.push_str("}\n\n");
    generated.push_str(
        "fn symbol_address(symbol: *const u8) -> usize { symbol.addr() }\n\n\
         /// Resolves Board storage from native linker symbols emitted by `memory.x`.\n\
         pub fn board_storage_layout(\n\
             flash_base: usize,\n\
         ) -> Result<Stm32StorageLayout, LinkerRegionError> {\n\
             let filesystem = LinkerRegion::try_from_addresses(\n\
                 flash_base, symbol_address(core::ptr::addr_of!(FILESYSTEM_START)),\n\
                 symbol_address(core::ptr::addr_of!(FILESYSTEM_END)),\n\
             )?;\n\
             let database = LinkerRegion::try_from_addresses(\n\
                 flash_base, symbol_address(core::ptr::addr_of!(DATABASE_START)),\n\
                 symbol_address(core::ptr::addr_of!(DATABASE_END)),\n\
             )?;\n",
    );
    if web_assets.is_some() {
        generated.push_str(
            "    let web_assets = Some(LinkerRegion::try_from_addresses(\n\
                 flash_base, symbol_address(core::ptr::addr_of!(WEB_ASSETS_START)),\n\
                 symbol_address(core::ptr::addr_of!(WEB_ASSETS_END)),\n\
             )?);\n",
        );
    } else {
        generated.push_str("    let web_assets = None;\n");
    }
    generated.push_str("    Ok(Stm32StorageLayout::new(filesystem, web_assets, database))\n}\n");
    fs::write(output.join("stm32_layout.rs"), generated)?;
    Ok(())
}

fn symbol_pair<'a>(linker: &str, label: &'a str, role: &str) -> Result<&'a str, Box<dyn Error>> {
    validate_symbol(label)?;
    let start = format!("__{label}_start");
    let end = format!("__{label}_end");
    if !linker.contains(&start) || !linker.contains(&end) {
        return Err(
            format!("{role} binding `{label}` has no `{start}`/`{end}` pair in memory.x").into(),
        );
    }
    Ok(label)
}

fn render_symbols(role: &str, label: &str) -> String {
    format!(
        "    #[link_name = \"__{label}_start\"]\n    static {role}_START: u8;\n\
         #[link_name = \"__{label}_end\"]\n    static {role}_END: u8;\n"
    )
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
