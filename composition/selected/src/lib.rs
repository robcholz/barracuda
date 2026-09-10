//! Thin composition of independently selected Platform and Board HAL resources.

#![no_std]

pub use barracuda_target_api::{TargetBindings, TargetResources};

#[doc(hidden)]
pub use barracuda_board_selected::__barracuda_generated_board_bindings as __board_bindings;
#[doc(hidden)]
pub use barracuda_board_selected::Bindings as __BoardBindings;

#[doc(hidden)]
pub use barracuda_platform_selected as __platform;

/// Complete move-only bindings required by the selected target axes.
pub type Bindings =
    TargetBindings<barracuda_platform_selected::Bindings, barracuda_board_selected::Bindings>;

/// Resources produced for the independently selected target axes.
pub type Resources =
    TargetResources<barracuda_platform_selected::Resources, barracuda_board_selected::Resources>;

/// Failure while composing the independently selected target axes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The selected Platform failed to initialize its mechanisms.
    #[error("selected Platform initialization failed: {0}")]
    Platform(barracuda_platform_selected::Error),
    /// The selected Board HAL failed to initialize its peripherals.
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

/// Constructs the statically selected host Target bindings for the application entry.
#[doc(hidden)]
#[must_use]
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn __application_bindings() -> Bindings {
    TargetBindings::new(&barracuda_board_selected::BOARD, ())
}

/// Generates the selected Platform ABI entry and connects it to `application`.
///
/// The callback is a direct path substituted at compile time. The expanded
/// entry contains no dynamic dispatch, boxed future, or runtime lookup.
#[macro_export]
macro_rules! application_entry {
    ($application:path) => {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        $crate::__platform::platform_entry!(|spawner| async {
            $application(spawner, $crate::__application_bindings()).await
        });

        #[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
        use $crate::__board_bindings;

        #[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
        $crate::__platform::platform_entry!(
            &$crate::BOARD,
            __board_bindings,
            $crate::__BoardBindings,
            |spawner, platform_bindings, board_bindings| async {
                $application(
                    spawner,
                    $crate::TargetBindings::new(platform_bindings, board_bindings),
                )
                .await
            }
        );
    };
}

/// Board selected independently from Platform.
pub use barracuda_board_selected::BOARD;
/// Platform name selected independently from Board HAL.
pub use barracuda_platform_selected::PLATFORM_NAME;
