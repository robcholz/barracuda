//! Thin composition of independently selected Platform and Board HAL resources.

#![no_std]

pub use barracuda_target_api::{TargetBindings, TargetIdentity, TargetResources};

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

/// Fixed identity of the independently selected Target axes.
pub const TARGET_IDENTITY: TargetIdentity = TargetIdentity::new(
    barracuda_platform_selected::PLATFORM_INFO,
    barracuda_board_selected::BOARD_INFO,
);

/// Static receive slots the application reserves: the largest long-lived
/// connection budget the selected Platform declares for any Board. The
/// selected Board's runtime limit (`TARGET_IDENTITY.long_lived_connections()`)
/// is at most this.
pub const RECEIVE_SLOTS: usize = barracuda_platform_selected::LONG_LIVED_CONNECTIONS;

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

/// Generates the selected Platform ABI entry and connects it to `application`.
///
/// The Platform entry acquires its peripheral singleton once (a chip's on a
/// device, the virtual peripherals on a host), moves the selected Board's
/// tokens out of it, and calls `application` with the resulting Target
/// bindings. The callback is a direct path substituted at compile time. The
/// expanded entry contains no dynamic dispatch, boxed future, or runtime
/// lookup.
#[macro_export]
macro_rules! application_entry {
    ($application:path) => {
        use $crate::__board_bindings;

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
