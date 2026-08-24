//! macOS Platform storage initialization against a generated-style Board descriptor.

#![allow(clippy::expect_used)]

use barracuda_board::{Board, Hardware, NativeLayout};
use barracuda_platform_macos::{
    FileLayout, FileRegion, MacosPlatform, MacosPlatformError, MacosSettings,
};

const REGIONS: &[FileRegion] = &[
    FileRegion::read_write("fs", 0, 4096),
    FileRegion::read_only("assets", 4096, 4096),
    FileRegion::read_write("kv", 8192, 4096),
];
const LAYOUT: FileLayout = FileLayout::new(12_288, REGIONS);
static BOARD: Board = Board::new(
    "macos-test",
    Hardware::new("macos"),
    NativeLayout::new("file-layout.yml"),
);
static LINUX_BOARD: Board = Board::new(
    "wrong-platform",
    Hardware::new("linux"),
    NativeLayout::new("file-layout.yml"),
);

#[test]
fn macos_platform_installs_its_os_reactor_for_embassy() {
    MacosPlatform::install_reactor().expect("install Tokio reactor");
    assert!(tokio::runtime::Handle::try_current().is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn macos_platform_realizes_board_native_region_names() {
    let directory = tempfile::tempdir().expect("temporary macOS state directory");
    let root = Box::leak(
        directory
            .path()
            .to_string_lossy()
            .into_owned()
            .into_boxed_str(),
    );
    let settings = MacosSettings::new(root, "board.flash");
    let partitions = MacosPlatform::initialize_partitions_with_layout(&BOARD, &settings, &LAYOUT)
        .await
        .expect("initialize macOS Platform storage");
    assert_eq!(partitions.len(), 3);
    assert!(partitions.get("kv").is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn macos_rejects_a_linux_board() {
    let settings = MacosSettings::new("unused", "unused.flash");
    assert!(matches!(
        MacosPlatform::initialize_partitions_with_layout(&LINUX_BOARD, &settings, &LAYOUT).await,
        Err(MacosPlatformError::IncompatibleChip { chip: "linux" })
    ));
}
