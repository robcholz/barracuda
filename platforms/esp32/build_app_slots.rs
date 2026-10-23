//! Checks the app slots of a Board's native ESP-IDF partition table.
//!
//! Shared by the build scripts of every ESP Platform, which include it with
//! `#[path]`. A table has one or more OTA app slots, `ota_0` first and the
//! rest numbered on without a gap, an `otadata` partition, and no `factory`
//! or `test` app:
//!
//! - `cargo run` and the release image write `ota_0`, and the web installer
//!   reads the app descriptor there.
//! - The ESP-IDF bootloader starts `ota_0` while `otadata` is erased, as the
//!   release image leaves it, when the table has no `factory` app; with one it
//!   would start `factory` instead. With one slot it starts `ota_0` whatever
//!   `otadata` holds: it picks the slot as the sequence number modulo the
//!   number of slots.
//! - It indexes slots by their subtype number, so a gap would hide a slot.
//!
//! Boards with 16 MiB of flash have two slots; smaller ones have one that
//! takes the room of the second.

use esp_idf_part::{AppType, DataType, PartitionTable, SubType, Type};

/// Returns the number of OTA app slots, or why the table cannot boot as
/// Barracuda installs it.
pub(crate) fn app_slot_count(table: &PartitionTable) -> Result<usize, String> {
    let first = u8::from(SubType::App(AppType::Ota_0));
    let last = u8::from(SubType::App(AppType::Ota_15));
    let mut slots = Vec::new();
    for partition in table.partitions() {
        if partition.ty() != Type::App {
            continue;
        }
        let subtype = u8::from(partition.subtype());
        if !(first..=last).contains(&subtype) {
            return Err(format!(
                "has the app partition `{}`, which is not an OTA slot; Barracuda installs its \
                 app in `ota_0`, and the bootloader would start a `factory` app instead",
                partition.name()
            ));
        }
        slots.push(subtype - first);
    }
    if slots.is_empty() {
        return Err(String::from("has no `ota_0` app slot"));
    }
    slots.sort_unstable();
    for (expected, slot) in slots.iter().enumerate() {
        if usize::from(*slot) != expected {
            return Err(format!(
                "numbers its app slots {}; they must run from `ota_0` without a gap",
                slots
                    .iter()
                    .map(|slot| format!("`ota_{slot}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if table
        .find_by_subtype(Type::Data, SubType::Data(DataType::Ota))
        .is_none()
    {
        return Err(String::from(
            "has no `otadata` partition, which the bootloader reads to pick an app slot",
        ));
    }
    Ok(slots.len())
}
