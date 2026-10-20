//! The link under the Platform's IP stack.
//!
//! The selected Board decides which link exists: its `hardware.ethernet`
//! declaration gives the stack a wired RMII Ethernet link; a Board without a
//! link the Platform drives gets a stack whose link is always down. Each link
//! lives in its own module beside this one and is chosen at build time by the
//! configuration `build.rs` projects from the Board, so a second link, such as
//! a Wi-Fi co-processor, is one more module and one more arm here.

use embassy_executor::Spawner;
use embassy_net::Stack;

use crate::{application::NetworkTokens, Esp32P4PlatformError};

#[cfg(barracuda_esp32p4_ethernet)]
pub(crate) mod ethernet;
#[cfg(not(barracuda_esp32p4_ethernet))]
mod unavailable;

/// Starts the Board's link and the task that runs the IP stack over it.
///
/// # Errors
///
/// Returns an error when the link hardware cannot be brought up or the stack
/// task cannot be allocated.
pub(crate) fn start(
    spawner: Spawner,
    tokens: NetworkTokens,
) -> Result<Stack<'static>, Esp32P4PlatformError> {
    #[cfg(barracuda_esp32p4_ethernet)]
    {
        ethernet::start(spawner, tokens.into_ethernet())
    }
    #[cfg(not(barracuda_esp32p4_ethernet))]
    {
        let NetworkTokens = tokens;
        unavailable::start(spawner)
    }
}
