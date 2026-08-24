//! Validates the selected Board against its native ESP-IDF partition table.

use std::{env, error::Error, fs, path::PathBuf};

use barracuda_board_config::{parse, read_selected_board, SELECTED_BOARD_PATH};
use esp_idf_part::{AppType, Flags, Partition, PartitionTable, SubType, Type};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..");
    let selection_path = root.join(SELECTED_BOARD_PATH);
    println!("cargo:rerun-if-changed={}", selection_path.display());
    let board_name = if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("riscv32") {
        read_selected_board(&root)?.ok_or("no Board selected; run `cargo board select` first")?
    } else {
        "esp32c6-devkitc-1".into()
    };
    let bundle = root.join("boards/configs").join(&board_name);
    let board_path = bundle.join("board.yml");
    println!("cargo:rerun-if-changed={}", board_path.display());

    let board = parse(&fs::read_to_string(board_path)?)?;
    let table_path = bundle.join(board.native_layout().artifact());
    println!("cargo:rerun-if-changed={}", table_path.display());
    if board.name() != board_name {
        return Err("Board bundle directory and Board name differ".into());
    }
    if board.hardware().chip() != "esp32c6" {
        return Err(format!(
            "ESP32-C6 Platform does not support chip `{}`",
            board.hardware().chip()
        )
        .into());
    }

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

    let mut generated = String::from("const BOARD_ESP32C6_REGIONS: &[Esp32c6Region] = &[\n");
    for partition in table.partitions() {
        generated.push_str(&render_region(partition));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "/// Complete partition projection validated from the selected Board's native ESP-IDF table.\n\
         pub const BOARD_ESP32C6_PARTITION_TABLE: Esp32c6PartitionTable = Esp32c6PartitionTable::new(\n\
             {:?}, {ota_slots}, BOARD_ESP32C6_REGIONS,\n\
         );\n",
        board.hardware().chip()
    ));

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    fs::write(output.join("esp32c6_layout.rs"), generated)?;
    Ok(())
}

fn render_region(partition: &Partition) -> String {
    let access = if partition.flags().contains(Flags::READONLY) {
        "Esp32c6RegionAccess::ReadOnly"
    } else {
        "Esp32c6RegionAccess::ReadWrite"
    };
    format!(
        "    Esp32c6Region::new({:?}, {}, {}, {access}),\n",
        partition.name(),
        partition.offset(),
        partition.size()
    )
}
