//! Linux Platform storage initialization against a generated-style Board descriptor.

#![allow(clippy::expect_used)]

use barracuda_board::{Board, Hardware, NativeLayout};
use barracuda_platform::PartitionFilesystem;
use barracuda_platform_linux::{
    FileLayout, FileRegion, LinuxPlatform, LinuxPlatformError, LinuxSettings,
};
use barracuda_tls::ClientTls as _;

const REGIONS: &[FileRegion] = &[
    FileRegion::read_write("fs", 0, 4096, PartitionFilesystem::LittleFs),
    FileRegion::read_only("assets", 4096, 4096, PartitionFilesystem::FatFs),
    FileRegion::read_write("kv", 8192, 4096, PartitionFilesystem::Raw),
];
const LAYOUT: FileLayout = FileLayout::new(12_288, REGIONS);
static BOARD: Board = Board::new(
    "linux-test",
    Hardware::new("linux"),
    NativeLayout::new("file-layout.yml"),
);
static MACOS_BOARD: Board = Board::new(
    "wrong-platform",
    Hardware::new("macos"),
    NativeLayout::new("file-layout.yml"),
);

#[test]
fn linux_platform_installs_its_os_reactor_for_embassy() {
    LinuxPlatform::install_reactor().expect("install Tokio reactor");
    assert!(tokio::runtime::Handle::try_current().is_ok());
}

#[test]
fn linux_platform_owns_host_tls_initialization() {
    let tls = LinuxPlatform::initialize_tls().expect("initialize Linux TLS");
    assert!(tls.config().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn linux_platform_realizes_board_native_region_names() {
    let directory = tempfile::tempdir().expect("temporary Linux state directory");
    let root = Box::leak(
        directory
            .path()
            .to_string_lossy()
            .into_owned()
            .into_boxed_str(),
    );
    let settings = LinuxSettings::new(root, "board.flash", "barracuda-test0");
    let partitions = LinuxPlatform::initialize_partitions_with_layout(&BOARD, &settings, &LAYOUT)
        .await
        .expect("initialize Linux Platform storage");
    assert_eq!(partitions.len(), 3);
    assert!(partitions.get("kv").is_some());
    assert_eq!(
        partitions
            .get("assets")
            .map(|partition| partition.filesystem()),
        Some(PartitionFilesystem::FatFs)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn linux_rejects_a_macos_board() {
    let settings = LinuxSettings::new("unused", "unused.flash", "unused0");
    assert!(matches!(
        LinuxPlatform::initialize_partitions_with_layout(&MACOS_BOARD, &settings, &LAYOUT).await,
        Err(LinuxPlatformError::IncompatibleChip { chip: "macos" })
    ));
}
