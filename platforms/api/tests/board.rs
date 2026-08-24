//! Board/Platform ownership boundary tests.

use core::{convert::Infallible, future::pending};

use barracuda_board::{Board, Hardware, Storage};
use barracuda_platform::{Platform, PlatformInitResult};
use barracuda_platform_test::{MemFs, MemoryPartition, NeverStack};
use embassy_executor::Spawner;

static BOARD: Board = Board::new(
    "example",
    Hardware::new("host"),
    Storage::new("datafs", None, "ekv"),
);

struct ExamplePlatform;

impl Platform for ExamplePlatform {
    type Network = NeverStack;
    type FileSystem = MemFs;
    type DatabaseRegion = MemoryPartition;
    type ModelApiFactory = ();
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _board: &'static Board) -> PlatformInitResult<Self> {
        pending().await
    }
}

#[test]
fn board_and_platform_are_independent_inputs() {
    fn assert_platform<P: Platform>() {}

    assert_platform::<ExamplePlatform>();
    assert_eq!(BOARD.storage().database(), "ekv");
}

#[test]
fn device_platforms_do_not_export_raw_filesystem_regions() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in ["platforms/esp32/src/lib.rs", "platforms/stm32/src/lib.rs"] {
        let source = std::fs::read_to_string(root.join(relative))?;
        assert!(
            !source.contains("FilesystemFlash") && !source.contains("filesystem_flash"),
            "{relative} exposes a raw filesystem region instead of a mounted FileSystem"
        );
    }
    Ok(())
}
