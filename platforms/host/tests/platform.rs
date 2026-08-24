//! Host Platform initialization against a generated-style Board descriptor.

#![allow(clippy::expect_used)]

use barracuda_board::{Board, Hardware, Storage};
use barracuda_platform_host::{
    HostLayout, HostPlatform, HostPlatformError, HostRegion, HostSettings,
};
use embedded_storage_async::nor_flash::ReadNorFlash as _;

const REGIONS: &[HostRegion] = &[
    HostRegion::read_write("fs", 0, 4096),
    HostRegion::read_only("assets", 4096, 4096),
    HostRegion::read_write("kv", 8192, 4096),
];
const LAYOUT: HostLayout = HostLayout::new(12_288, REGIONS);
static BOARD: Board = Board::new(
    "host-test",
    Hardware::new("host"),
    Storage::new("fs", Some("assets"), "kv"),
);
static ESP_BOARD: Board = Board::new(
    "wrong-platform",
    Hardware::new("esp32c6"),
    Storage::new("fs", Some("assets"), "kv"),
);

#[test]
fn host_platform_installs_its_os_reactor_for_embassy() {
    HostPlatform::install_reactor().expect("install Tokio reactor");
    assert!(tokio::runtime::Handle::try_current().is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn host_platform_realizes_board_native_region_names() {
    let directory = tempfile::tempdir().expect("temporary Host state directory");
    let root = Box::leak(
        directory
            .path()
            .to_string_lossy()
            .into_owned()
            .into_boxed_str(),
    );
    let settings = HostSettings::new(root, "board.flash");

    let resources = HostPlatform::initialize_with_layout(&BOARD, &settings, &LAYOUT)
        .await
        .expect("initialize Host Platform");

    assert_eq!(resources.database_region.capacity(), 4096);
}

#[tokio::test(flavor = "current_thread")]
async fn independently_selected_host_rejects_an_incompatible_board() {
    let settings = HostSettings::new("unused", "unused.flash");
    assert!(matches!(
        HostPlatform::initialize_with_layout(&ESP_BOARD, &settings, &LAYOUT).await,
        Err(HostPlatformError::IncompatibleChip { chip: "esp32c6" })
    ));
}
