//! Validates the selected Board against its native ESP-IDF partition table.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::parse;
use esp_idf_part::{AppType, Flags, Partition, PartitionTable, SubType, Type};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_BOARD");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let board_name = env::var("BARRACUDA_BOARD").unwrap_or_else(|_| "esp32c6-devkitc-1".into());
    validate_name(&board_name)?;
    let bundle = root.join("boards/configs").join(&board_name);
    let board_path = bundle.join("board.yml");
    let table_path = bundle.join("partitions.csv");
    println!("cargo:rerun-if-changed={}", board_path.display());
    println!("cargo:rerun-if-changed={}", table_path.display());

    let board = parse(&fs::read_to_string(board_path)?)?;
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }
    if board.hardware().chip() != "esp32c6" {
        return Err(format!(
            "ESP32 example currently enables esp32c6, not `{}`",
            board.hardware().chip()
        )
        .into());
    }

    let table = PartitionTable::try_from_str(fs::read_to_string(table_path)?)?;
    table.validate()?;
    let database = writable(&table, board.storage().database(), "database")?;
    let filesystem = writable(&table, board.storage().filesystem(), "filesystem")?;
    let web_assets = board
        .storage()
        .web_assets()
        .map(|name| readonly(&table, name, "web-assets"))
        .transpose()?;
    for slot in [AppType::Ota_0, AppType::Ota_1] {
        if table
            .find_by_subtype(Type::App, SubType::App(slot))
            .is_none()
        {
            return Err(format!("ESP32 native table is missing `{slot:?}`").into());
        }
    }
    let ota_slots = table
        .partitions()
        .iter()
        .filter(|partition| {
            partition.ty() == Type::App
                && matches!(
                    partition.subtype(),
                    SubType::App(slot) if slot != AppType::Factory && slot != AppType::Test
                )
        })
        .count();

    let mut generated = String::new();
    generated.push_str(&render_region("BOARD_DATABASE", database));
    generated.push_str(&render_region("BOARD_FILESYSTEM", filesystem));
    let assets = if let Some(region) = web_assets {
        generated.push_str(&render_region("BOARD_WEB_ASSETS", region));
        "Some(BOARD_WEB_ASSETS)"
    } else {
        "None"
    };
    generated.push_str(&format!(
        "/// Storage projection validated from the selected Board's native ESP-IDF table.\n\
         pub const BOARD_ESP32_LAYOUT: Esp32StorageLayout = Esp32StorageLayout::new(\n\
             {:?}, {ota_slots}, BOARD_DATABASE, BOARD_FILESYSTEM, {assets},\n\
         );\n",
        board.hardware().chip()
    ));

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("esp32_layout.rs"), generated)?;
    Ok(())
}

fn writable<'a>(
    table: &'a PartitionTable,
    name: &str,
    role: &str,
) -> Result<&'a Partition, Box<dyn Error>> {
    let partition = table
        .find(name)
        .ok_or_else(|| format!("{role} binding `{name}` is absent from partitions.csv"))?;
    if partition.flags().contains(Flags::READONLY) {
        return Err(format!("{role} binding `{name}` is read-only").into());
    }
    Ok(partition)
}

fn readonly<'a>(
    table: &'a PartitionTable,
    name: &str,
    role: &str,
) -> Result<&'a Partition, Box<dyn Error>> {
    let partition = table
        .find(name)
        .ok_or_else(|| format!("{role} binding `{name}` is absent from partitions.csv"))?;
    if !partition.flags().contains(Flags::READONLY) {
        return Err(format!("{role} binding `{name}` must carry ESP-IDF readonly flag").into());
    }
    Ok(partition)
}

fn render_region(name: &str, partition: &Partition) -> String {
    let access = if partition.flags().contains(Flags::READONLY) {
        "Esp32RegionAccess::ReadOnly"
    } else {
        "Esp32RegionAccess::ReadWrite"
    };
    format!(
        "const {name}: Esp32Region = Esp32Region::new({:?}, {}, {}, {access});\n",
        partition.name(),
        partition.offset(),
        partition.size()
    )
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
