//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

mod entropy;

use barracuda_board_hal::{BoardResources, NoExposedIo, NoPeripherals};
pub use barracuda_platform::{Entropy, EntropyUnavailable};
pub use barracuda_target_api::{BoardInfo, Hardware, PlatformInfo, TargetIdentity};
pub use embassy_net::Stack;
pub use entropy::SharedEntropy;
pub use http_client::{ClientFactory, ReceiveLease, ReceiveSlots};
use portable_atomic_util::Arc;

/// Fixed System resources available while constructing a Plugin.
///
/// Plugins move owned resources or copy shared capabilities during `new` and
/// do not retain a reference to the context itself.
pub struct PluginContext<Peripherals = NoPeripherals, ExposedIo = NoExposedIo> {
    /// Fixed identity of the selected Platform and Board.
    pub target_identity: TargetIdentity,
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// Factory for constructing HTTP clients over the Platform network and TLS
    /// capabilities.
    pub http_clients: ClientFactory<'static>,
    /// Dedicated connection slots for long-lived receive loops, sized for the
    /// selected Target. Empty unless System assigns them.
    pub receive_slots: ReceiveSlots,
    /// The Platform's entropy source, fit for cryptographic use. Unavailable
    /// unless System assigns it.
    pub entropy: SharedEntropy,
    /// Complete HAL produced by the selected Board composition.
    pub hal: BoardResources<Peripherals, Arc<ExposedIo>>,
}

impl<Peripherals, ExposedIo> PluginContext<Peripherals, ExposedIo> {
    /// Creates the unified Plugin construction context with the selected HAL.
    #[must_use]
    pub fn from_hal(
        target_identity: TargetIdentity,
        ip_stack: Stack<'static>,
        http_clients: ClientFactory<'static>,
        hal: BoardResources<Peripherals, ExposedIo>,
    ) -> Self {
        let BoardResources {
            peripherals,
            exposed_io,
        } = hal;
        Self {
            target_identity,
            ip_stack,
            http_clients,
            receive_slots: ReceiveSlots::unavailable(),
            entropy: SharedEntropy::unavailable(),
            hal: BoardResources::new(peripherals, Arc::new(exposed_io)),
        }
    }
}

impl PluginContext {
    /// Creates a construction context carrying an explicitly empty HAL.
    #[must_use]
    pub fn new(
        target_identity: TargetIdentity,
        ip_stack: Stack<'static>,
        http_clients: ClientFactory<'static>,
    ) -> Self {
        Self::from_hal(
            target_identity,
            ip_stack,
            http_clients,
            BoardResources::new(NoPeripherals, NoExposedIo),
        )
    }
}
