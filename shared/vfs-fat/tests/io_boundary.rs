//! Compile-time source contract for the FAT device boundary.

#[test]
fn fat_backend_only_accepts_embedded_async_byte_io() {
    let manifest = include_str!("../Cargo.toml");
    let source = include_str!("../src/lib.rs");

    for forbidden in [
        "block-device-driver",
        "block_device_driver",
        "BlockDeviceIo",
        "mount_block_device",
        "embedded-storage",
        "embedded_storage",
        "ReadOnlyPartitionIo",
        "ReadNorFlash",
    ] {
        assert!(
            !manifest.contains(forbidden) && !source.contains(forbidden),
            "vfs-fat must not expose or depend on storage-specific boundary `{forbidden}`"
        );
    }

    assert!(manifest.contains("embedded-io-async"));
    assert!(source.contains("use embedded_io_async::{Read, Seek, Write};"));
}
