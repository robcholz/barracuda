//! Thin composition of independently selected Platform and Board HAL resources.

#![no_std]

pub use barracuda_target_api::{TargetBindings, TargetResources};

/// Complete move-only bindings required by the selected target axes.
pub type Bindings =
    TargetBindings<barracuda_platform_selected::Bindings, barracuda_board_selected::Bindings>;

/// Resources produced for the independently selected target axes.
pub type Resources =
    TargetResources<barracuda_platform_selected::Resources, barracuda_board_selected::Resources>;

/// Splits the ESP32-C5 peripheral singleton into the independently selected
/// Platform and Board HAL bindings.
///
/// The selected C5 DevKit board currently declares no exposed GPIO, I2C, or
/// SPI resources, so every chip mechanism is assigned to the Platform axis.
///
/// # Errors
///
/// Returns an error when the selected Board does not target ESP32-C5.
#[cfg(all(target_arch = "riscv32", feature = "esp32c5"))]
pub fn bindings_from_peripherals(
    peripherals: esp_hal::peripherals::Peripherals,
) -> Result<Bindings, barracuda_platform_selected::Error> {
    let platform = barracuda_platform_selected::Bindings::new(
        &BOARD,
        peripherals.FLASH,
        peripherals.WIFI,
        peripherals.TIMG0,
        peripherals.FROM_CPU_INTR0,
    )?;
    Ok(TargetBindings::new(platform, ()))
}

/// Failure while composing the independently selected target axes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The selected Platform failed to initialize its mechanisms.
    #[error("selected Platform initialization failed: {0}")]
    Platform(barracuda_platform_selected::Error),
    /// The selected Board HAL failed to initialize its peripheral Drivers.
    #[error("selected Board HAL initialization failed: {0}")]
    BoardHal(barracuda_board_selected::Error),
}

/// Constructs the independently selected Platform and Board HAL resources.
///
/// # Errors
///
/// Returns the error from the axis whose initialization failed.
pub async fn resources_with_bindings(
    spawner: embassy_executor::Spawner,
    bindings: Bindings,
) -> Result<Resources, Error> {
    barracuda_platform_selected::prepare().map_err(Error::Platform)?;
    let (platform_bindings, board_bindings) = bindings.split();
    let board_hal = barracuda_board_selected::resources(spawner, board_bindings)
        .await
        .map_err(Error::BoardHal)?;
    let platform = barracuda_platform_selected::resources(spawner, platform_bindings)
        .await
        .map_err(Error::Platform)?;
    Ok(TargetResources {
        platform,
        board_hal,
    })
}

/// Constructs resources for the local host Target.
///
/// Host Platforms do not own a chip peripheral singleton. Device entry points
/// call [`resources_with_bindings`] after splitting their singleton instead.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub async fn resources(spawner: embassy_executor::Spawner) -> Result<Resources, Error> {
    resources_with_bindings(
        spawner,
        TargetBindings::new(&barracuda_board_selected::BOARD, ()),
    )
    .await
}

/// Board selected independently from Platform.
pub use barracuda_board_selected::BOARD;
/// Platform name selected independently from Board HAL.
pub use barracuda_platform_selected::PLATFORM_NAME;
