//! The links under the Platform's IP stack and its Wi-Fi device.
//!
//! The selected Board decides which links exist, and `build.rs` projects them
//! into configuration this module is chosen by:
//!
//! - `hardware.ethernet` gives the stack a wired RMII Ethernet link
//!   (`ethernet`);
//! - `hardware.wifi-coprocessor` gives the Platform a Wi-Fi radio on an
//!   ESP-Hosted co-processor over SDIO (`coprocessor`): the IP stack runs
//!   over its station, a second stack over its soft access point, and the
//!   Platform's `WifiDevice` drives it;
//! - a Board with both runs the one IP stack over whichever link is up,
//!   preferring Ethernet (`failover`); the co-processor's station joins
//!   networks and its access point serves set-up either way;
//! - a Board with neither gets a stack whose link is always down
//!   (`unavailable`).

use embassy_executor::Spawner;
use embassy_net::Stack;

use crate::{application::NetworkTokens, Esp32P4PlatformError};

#[cfg(barracuda_esp32p4_wifi_coprocessor)]
pub(crate) mod coprocessor;
#[cfg(barracuda_esp32p4_ethernet)]
pub(crate) mod ethernet;
#[cfg(all(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor))]
mod failover;
#[cfg(not(any(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor)))]
mod unavailable;

/// The Platform's Wi-Fi device: the co-processor when the Board has one.
#[cfg(barracuda_esp32p4_wifi_coprocessor)]
pub(crate) type Wifi = coprocessor::CoprocessorWifi;
/// The Platform's Wi-Fi device: the Board has no radio.
#[cfg(not(barracuda_esp32p4_wifi_coprocessor))]
pub(crate) type Wifi = barracuda_platform::UnavailableWifiDevice;

/// The IP stack and the Wi-Fi device the Board's links produce.
pub(crate) struct NetworkLinks {
    /// The stack System and Plugins share.
    pub(crate) ip_stack: Stack<'static>,
    /// The Wi-Fi device.
    pub(crate) wifi: Wifi,
}

/// Starts the Board's links and the tasks that run the IP stacks over them.
///
/// # Errors
///
/// Returns an error when link hardware cannot be brought up or a stack task
/// cannot be allocated.
pub(crate) fn start(
    spawner: Spawner,
    tokens: NetworkTokens,
) -> Result<NetworkLinks, Esp32P4PlatformError> {
    #[cfg(all(barracuda_esp32p4_ethernet, not(barracuda_esp32p4_wifi_coprocessor)))]
    {
        let ip_stack = ethernet::start(spawner, tokens.ethernet)?;
        Ok(NetworkLinks {
            ip_stack,
            wifi: barracuda_platform::UnavailableWifiDevice::new(ip_stack),
        })
    }
    #[cfg(all(barracuda_esp32p4_wifi_coprocessor, not(barracuda_esp32p4_ethernet)))]
    {
        let coprocessor = coprocessor::start(spawner, tokens.coprocessor)?;
        let ip_stack = coprocessor::station_stack(spawner, coprocessor.station)?;
        Ok(NetworkLinks {
            ip_stack,
            wifi: coprocessor.radio.into_wifi(spawner, ip_stack)?,
        })
    }
    #[cfg(all(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor))]
    {
        let wired = ethernet::link(tokens.ethernet)?;
        let coprocessor = coprocessor::start(spawner, tokens.coprocessor)?;
        let ip_stack = failover::start(spawner, wired, coprocessor.station)?;
        Ok(NetworkLinks {
            ip_stack,
            wifi: coprocessor.radio.into_wifi(spawner, ip_stack)?,
        })
    }
    #[cfg(not(any(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor)))]
    {
        let NetworkTokens = tokens;
        let ip_stack = unavailable::start(spawner)?;
        Ok(NetworkLinks {
            ip_stack,
            wifi: barracuda_platform::UnavailableWifiDevice::new(ip_stack),
        })
    }
}

/// The chip's factory base MAC with the multicast bit cleared, in case a
/// custom eFuse value set it. The ESP32-P4 has no radio, so the EMAC may use
/// it; a co-processor brings its own.
#[cfg(barracuda_esp32p4_ethernet)]
pub(crate) fn base_mac() -> [u8; 6] {
    let mut mac = [0; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    mac[0] &= !1;
    mac
}

/// Seeds the IP stacks' sequence numbers. Without a hardware random source
/// the seed comes from the chip's base MAC, which differs per device.
#[cfg(any(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor))]
pub(crate) fn stack_seed() -> u64 {
    esp_hal::efuse::base_mac_address()
        .as_bytes()
        .iter()
        .fold(0_u64, |seed, byte| (seed << 8) | u64::from(*byte))
}

/// Logs each address a network gives the device, so a person at the console
/// knows where to reach it.
#[cfg(any(barracuda_esp32p4_ethernet, barracuda_esp32p4_wifi_coprocessor))]
pub(crate) async fn report_addresses(link: &str, stack: Stack<'static>) {
    loop {
        stack.wait_config_up().await;
        if let Some(config) = stack.config_v4() {
            log::info!("{link} is up at {}", config.address.address());
        }
        stack.wait_config_down().await;
        log::info!("{link} is down");
    }
}
