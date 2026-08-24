//! Thin composition of independently selected Platform and Board HAL resources.

#![no_std]

pub use barracuda_target_api::TargetResources;

/// Resources produced for the independently selected target axes.
pub type Resources =
    TargetResources<barracuda_platform_selected::Resources, barracuda_board_selected::Resources>;

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
pub async fn resources(spawner: embassy_executor::Spawner) -> Result<Resources, Error> {
    barracuda_platform_selected::prepare().map_err(Error::Platform)?;
    let board_hal = barracuda_board_selected::resources(spawner)
        .await
        .map_err(Error::BoardHal)?;
    let platform =
        barracuda_platform_selected::resources(spawner, &barracuda_board_selected::BOARD)
            .await
            .map_err(Error::Platform)?;
    Ok(TargetResources {
        platform,
        board_hal,
    })
}

/// Board selected independently from Platform.
pub use barracuda_board_selected::BOARD;
/// Platform name selected independently from Board HAL.
pub use barracuda_platform_selected::PLATFORM_NAME;
