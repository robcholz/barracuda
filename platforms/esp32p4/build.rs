//! Validates the selected Board against its native ESP-IDF partition table.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, SELECTED_BOARD_PATH};
use esp_idf_part::{AppType, DataType, Flags, Partition, PartitionTable, SubType, Type};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=BARRACUDA_LOG_LEVEL");
    let log_level = generated_log_level()?;
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

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);

    // This Platform is an unconditional dependency of the selected-Platform
    // composition, so its build script runs for every selected Board, not only
    // for esp32p4 Boards. When the selected Board targets a different chip this
    // Platform is inactive: emit an empty projection so the crate still compiles
    // as a dependency, without inspecting the build target or naming a fallback
    // Board. An esp32p4 build selects an esp32p4 Board and gets the real table.
    if board.hardware().chip() != "esp32p4" {
        let mut generated = inactive_layout();
        generated.push_str(&log_level);
        fs::write(output.join("esp32p4_layout.rs"), generated)?;
        return Ok(());
    }

    let compiler_builtins = manifest.join("compiler-builtins.S");
    println!("cargo:rerun-if-changed={}", compiler_builtins.display());
    cc::Build::new()
        .file(compiler_builtins)
        .compile("barracuda_esp32p4_compiler_builtins");

    let table_path = bundle.join(board.native_layout().artifact());
    println!("cargo:rerun-if-changed={}", table_path.display());
    let table = PartitionTable::try_from_str(fs::read_to_string(table_path)?)?;
    table.validate()?;
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

    let mut generated = String::from("const BOARD_ESP32P4_REGIONS: &[Esp32P4Region] = &[\n");
    for partition in table.partitions() {
        generated.push_str(&render_region(partition));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "/// Complete partition projection validated from the selected Board's native ESP-IDF table.\n\
         pub const BOARD_ESP32P4_PARTITION_TABLE: Esp32P4PartitionTable = Esp32P4PartitionTable::new(\n\
             {:?}, {ota_slots}, BOARD_ESP32P4_REGIONS,\n\
         );\n",
        board.hardware().chip()
    ));
    generated.push_str(&log_level);

    fs::write(output.join("esp32p4_layout.rs"), generated)?;
    Ok(())
}

fn generated_log_level() -> Result<String, Box<dyn Error>> {
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
    Ok(format!(
        "\n/// Log level selected at build time.\npub const PLATFORM_LOG_LEVEL: ::log::LevelFilter = ::log::LevelFilter::{variant};\n"
    ))
}

/// Projection emitted when the selected Board is not an esp32p4 Board. The
/// esp32p4 Platform is inactive for that build, so it exposes an empty table.
fn inactive_layout() -> String {
    String::from(
        "const BOARD_ESP32P4_REGIONS: &[Esp32P4Region] = &[];\n\
         /// Inactive projection: the selected Board targets another chip.\n\
         pub const BOARD_ESP32P4_PARTITION_TABLE: Esp32P4PartitionTable = Esp32P4PartitionTable::new(\n\
             \"esp32p4\", 0, BOARD_ESP32P4_REGIONS,\n\
         );\n",
    )
}

fn render_region(partition: &Partition) -> String {
    let access = if partition.flags().contains(Flags::READONLY) {
        "Esp32P4RegionAccess::ReadOnly"
    } else {
        "Esp32P4RegionAccess::ReadWrite"
    };
    let filesystem = match partition.subtype() {
        SubType::Data(DataType::Fat) => "barracuda_platform::PartitionFilesystem::FatFs",
        SubType::Data(DataType::Littlefs) => "barracuda_platform::PartitionFilesystem::LittleFs",
        _ => "barracuda_platform::PartitionFilesystem::Raw",
    };
    format!(
        "    Esp32P4Region::new({:?}, {}, {}, {access}, {filesystem}),\n",
        partition.name(),
        partition.offset(),
        partition.size()
    )
}
