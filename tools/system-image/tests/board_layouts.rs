//! Every ESP Board's partition table fits the flash the Board declares.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use esp_idf_part::PartitionTable;

#[test]
fn every_esp_partition_table_fits_its_declared_flash() {
    let configs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards/configs");
    let mut problems = Vec::new();
    for entry in fs::read_dir(&configs).expect("Board configs") {
        let bundle = entry.expect("Board bundle").path();
        let table_path = bundle.join("partitions.csv");
        if !table_path.is_file() {
            continue;
        }
        let name = bundle
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let name = name.unwrap_or_default();
        let board = barracuda_board_config::parse(
            &fs::read_to_string(bundle.join("board.yml")).expect("board.yml"),
        )
        .expect("valid board.yml");
        let table =
            PartitionTable::try_from_str(fs::read_to_string(&table_path).expect("partitions.csv"))
                .expect("valid partition table");
        let end = table
            .partitions()
            .iter()
            .map(|partition| u64::from(partition.offset()) + u64::from(partition.size()))
            .max()
            .unwrap_or(0);
        let Some(declared) = board.hardware().flash_size() else {
            problems.push(format!("{name}: declares no hardware.flash-size"));
            continue;
        };
        let Some(mebibytes) = declared
            .strip_suffix("mb")
            .and_then(|size| size.parse::<u64>().ok())
        else {
            problems.push(format!("{name}: unreadable flash-size `{declared}`"));
            continue;
        };
        if end > mebibytes * 1024 * 1024 {
            problems.push(format!(
                "{name}: partition table ends at {end:#x}, past its {declared} flash"
            ));
        }
    }
    problems.sort();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
