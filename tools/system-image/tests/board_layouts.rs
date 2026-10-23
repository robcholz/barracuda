//! Every ESP Board's partition table fits the flash the Board declares and
//! has the app slots the release image, `cargo run`, and the bootloader use.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use esp_idf_part::{AppType, DataType, PartitionTable, SubType, Type};

/// One ESP Board: its name, declared flash size, and native table.
struct EspBoard {
    name: String,
    flash_size: Option<String>,
    table: PartitionTable,
}

fn esp_boards() -> Vec<EspBoard> {
    let configs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards/configs");
    let mut boards = Vec::new();
    for entry in fs::read_dir(&configs).expect("Board configs") {
        let bundle = entry.expect("Board bundle").path();
        let table_path = bundle.join("partitions.csv");
        if !table_path.is_file() {
            continue;
        }
        let name = bundle
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let board = barracuda_board_config::parse(
            &fs::read_to_string(bundle.join("board.yml")).expect("board.yml"),
        )
        .expect("valid board.yml");
        let table =
            PartitionTable::try_from_str(fs::read_to_string(&table_path).expect("partitions.csv"))
                .expect("valid partition table");
        boards.push(EspBoard {
            name,
            flash_size: board.hardware().flash_size().map(str::to_owned),
            table,
        });
    }
    boards.sort_by(|a, b| a.name.cmp(&b.name));
    boards
}

/// The declared flash size in MiB, or why it cannot be read.
fn mebibytes(board: &EspBoard) -> Result<u64, String> {
    let Some(declared) = board.flash_size.as_deref() else {
        return Err(format!("{}: declares no hardware.flash-size", board.name));
    };
    declared
        .strip_suffix("mb")
        .and_then(|size| size.parse::<u64>().ok())
        .ok_or_else(|| format!("{}: unreadable flash-size `{declared}`", board.name))
}

#[test]
fn every_esp_partition_table_fits_its_declared_flash() {
    let mut problems = Vec::new();
    for board in esp_boards() {
        let end = board
            .table
            .partitions()
            .iter()
            .map(|partition| u64::from(partition.offset()) + u64::from(partition.size()))
            .max()
            .unwrap_or(0);
        match mebibytes(&board) {
            Ok(mebibytes) if end > mebibytes * 1024 * 1024 => problems.push(format!(
                "{}: partition table ends at {end:#x}, past its {}mb flash",
                board.name, mebibytes
            )),
            Ok(_) => {}
            Err(problem) => problems.push(problem),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Boards with 16 MiB of flash or more keep two OTA slots; smaller ones have
/// one, which takes the room of the second. Either way the release image and
/// `cargo run` write `ota_0`, the bootloader starts it while `otadata` is
/// erased because there is no `factory` app, and the release image, which
/// runs from 0x0 to the end of `ota_0`, erases only `nvs`, `otadata`, and
/// `phy_init` on the way, never the data regions behind the slots.
#[test]
fn every_esp_board_installs_its_app_in_ota_0() {
    let mut problems = Vec::new();
    for board in esp_boards() {
        let name = &board.name;
        let Ok(mebibytes) = mebibytes(&board) else {
            continue;
        };
        let partitions = board.table.partitions();
        let apps: Vec<_> = partitions
            .iter()
            .filter(|partition| partition.ty() == Type::App)
            .collect();
        let expected: Vec<_> = if mebibytes >= 16 {
            vec![SubType::App(AppType::Ota_0), SubType::App(AppType::Ota_1)]
        } else {
            vec![SubType::App(AppType::Ota_0)]
        };
        let found: Vec<_> = apps.iter().map(|partition| partition.subtype()).collect();
        if found != expected {
            problems.push(format!(
                "{name}: app partitions {found:?}, expected {expected:?} for {mebibytes} MiB"
            ));
            continue;
        }
        if board
            .table
            .find_by_subtype(Type::Data, SubType::Data(DataType::Ota))
            .is_none()
        {
            problems.push(format!("{name}: no `otadata` partition"));
        }
        let first = apps[0];
        let release_end = first.offset() + first.size();
        for partition in partitions {
            let below = partition.offset() < release_end;
            let erased = matches!(partition.name().as_str(), "nvs" | "otadata" | "phy_init");
            if below && partition.ty() == Type::Data && !erased {
                problems.push(format!(
                    "{name}: `{}` lies before the end of `ota_0`, where the release image erases it",
                    partition.name()
                ));
            }
        }
        if let Some(slot) = apps.get(1) {
            if slot.size() != first.size() {
                problems.push(format!("{name}: `ota_0` and `ota_1` differ in size"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
