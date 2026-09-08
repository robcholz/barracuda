//! Compile-only coverage for the Browser Platform application entry.

#![cfg(target_arch = "wasm32")]

barracuda_platform_browser::platform_entry!(|_spawner| async {
    Ok::<(), core::convert::Infallible>(())
});

#[allow(dead_code)]
fn assert_system_partition_bounds() {
    fn assert_partition<T: embedded_storage::nor_flash::NorFlash + Send + Unpin>() {}
    assert_partition::<barracuda_platform_browser::BrowserPartition>();
}
