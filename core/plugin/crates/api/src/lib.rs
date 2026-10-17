//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

mod entropy;
mod wifi;

use barracuda_board_hal::{BoardResources, NoExposedIo, NoPeripherals};
pub use barracuda_platform::{Entropy, EntropyUnavailable};
pub use barracuda_target_api::{BoardInfo, Hardware, PlatformInfo, TargetIdentity};
pub use embassy_net::Stack;
pub use entropy::SharedEntropy;
use portable_atomic_util::Arc;
pub use wifi::{PlatformWifi, PlatformWifiError};

/// Fixed System resources available while constructing a Plugin.
///
/// It carries only resources the Platform, Board, or System owns for the whole
/// Target. Plugins move owned resources or copy shared ones during `new` and
/// do not retain a reference to the context itself.
pub struct PluginContext<Peripherals = NoPeripherals, ExposedIo = NoExposedIo> {
    /// Fixed identity of the selected Platform and Board.
    pub target_identity: TargetIdentity,
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// The Platform's entropy source, fit for cryptographic use. Unavailable
    /// unless System assigns it.
    pub entropy: SharedEntropy,
    /// The Platform's Wi-Fi radio, taken by the one Plugin that owns Wi-Fi
    /// policy. Absent unless System assigns it.
    pub wifi: Option<PlatformWifi>,
    /// Complete HAL produced by the selected Board composition.
    pub hal: BoardResources<Peripherals, Arc<ExposedIo>>,
}

impl<Peripherals, ExposedIo> PluginContext<Peripherals, ExposedIo> {
    /// Creates the unified Plugin construction context with the selected HAL.
    #[must_use]
    pub fn from_hal(
        target_identity: TargetIdentity,
        ip_stack: Stack<'static>,
        hal: BoardResources<Peripherals, ExposedIo>,
    ) -> Self {
        let BoardResources {
            peripherals,
            exposed_io,
        } = hal;
        Self {
            target_identity,
            ip_stack,
            entropy: SharedEntropy::unavailable(),
            wifi: None,
            hal: BoardResources::new(peripherals, Arc::new(exposed_io)),
        }
    }
}

impl PluginContext {
    /// Creates a construction context carrying an explicitly empty HAL.
    #[must_use]
    pub fn new(target_identity: TargetIdentity, ip_stack: Stack<'static>) -> Self {
        Self::from_hal(
            target_identity,
            ip_stack,
            BoardResources::new(NoPeripherals, NoExposedIo),
        )
    }
}
