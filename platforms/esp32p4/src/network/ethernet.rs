//! Wired RMII Ethernet: the ESP32-P4 EMAC, the Board's PHY, and the IP stack
//! over them.

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_net::{Runner, Stack, StackResources};
use esp_hal::{
    delay::Delay,
    ethernet::{phy::generic::GenericPhy, Ethernet, EthernetDmaStorage, EthernetPinBundle},
    gpio::{AnyPin, Level, Output, OutputConfig},
    peripherals::ETH,
    Async,
};
use static_cell::{ConstStaticCell, StaticCell};

use crate::Esp32P4PlatformError;

/// Receive descriptors and frame buffers in internal RAM.
const RECEIVE_SLOTS: usize = 8;
/// Transmit descriptors and frame buffers in internal RAM.
const TRANSMIT_SLOTS: usize = 8;
/// How long the PHY reset is held low. The IP101GRI datasheet asks for at
/// least 10 ms.
const PHY_RESET_MS: u32 = 10;
/// How long the PHY needs after reset before its management registers answer.
const PHY_READY_MS: u32 = 10;

/// The wired link's IP stack as embassy-net drives it.
type EthernetLink = Ethernet<'static, Async, GenericPhy>;

/// Chip resources the Board's Ethernet consumes, built by the generated
/// network bindings from `hardware.ethernet`.
pub struct EthernetTokens<Pins> {
    /// The EMAC.
    pub(crate) eth: ETH<'static>,
    /// The RMII data, management, and reference-clock pins.
    pub(crate) pins: Pins,
    /// The pin wired to the PHY's active-low reset, when the Board has one.
    pub(crate) phy_reset: Option<AnyPin<'static>>,
    /// The PHY's Clause 22 management address.
    pub(crate) phy_address: u8,
}

#[embassy_executor::task]
async fn run_ethernet(mut runner: Runner<'static, EthernetLink>, stack: Stack<'static>) -> ! {
    let (never, ()) = join(runner.run(), report_addresses(stack)).await;
    never
}

/// Logs each address the wired network gives the device, so a person at the
/// console knows where to reach it.
async fn report_addresses(stack: Stack<'static>) {
    loop {
        stack.wait_config_up().await;
        if let Some(config) = stack.config_v4() {
            log::info!("Ethernet is up at {}", config.address.address());
        }
        stack.wait_config_down().await;
        log::info!("Ethernet link is down");
    }
}

/// Resets the PHY, brings the EMAC up over the Board's RMII wiring, and
/// starts the task that runs a DHCP client over it.
///
/// # Errors
///
/// Returns an error when the PHY does not answer on its address or the stack
/// task cannot be allocated.
pub(super) fn start<Pins: EthernetPinBundle + 'static>(
    spawner: Spawner,
    tokens: EthernetTokens<Pins>,
) -> Result<Stack<'static>, Esp32P4PlatformError> {
    static STORAGE: ConstStaticCell<EthernetDmaStorage<RECEIVE_SLOTS, TRANSMIT_SLOTS>> =
        ConstStaticCell::new(EthernetDmaStorage::new());
    static RESOURCES: StaticCell<StackResources<16>> = StaticCell::new();
    static PHY_RESET: StaticCell<Output<'static>> = StaticCell::new();

    if let Some(pin) = tokens.phy_reset {
        let delay = Delay::new();
        let reset = PHY_RESET.init(Output::new(pin, Level::Low, OutputConfig::default()));
        delay.delay_millis(PHY_RESET_MS);
        reset.set_high();
        delay.delay_millis(PHY_READY_MS);
    }

    // The ESP32-P4 has no radio, so its factory base MAC is free for the
    // EMAC. Clear the multicast bit in case a custom eFuse value set it.
    let mut mac = [0; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    mac[0] &= !1;
    let ethernet = Ethernet::new(
        tokens.eth,
        STORAGE.take(),
        mac,
        GenericPhy::new(tokens.phy_address),
        tokens.pins,
    )
    .map_err(Esp32P4PlatformError::Ethernet)?
    .into_async();
    log::info!(
        "Ethernet MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, PHY at address {}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        tokens.phy_address
    );

    // Without a hardware random source the stack seeds its sequence numbers
    // from the MAC, which differs per device.
    let seed = mac
        .iter()
        .fold(0_u64, |seed, byte| (seed << 8) | u64::from(*byte));
    let (stack, runner) = embassy_net::new(
        ethernet,
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(run_ethernet(runner, stack).map_err(|_error| Esp32P4PlatformError::NetworkTask)?);
    Ok(stack)
}
