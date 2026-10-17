//! Shows Wi-Fi in the system UI: the link on the status bar and the control
//! center, and the setup hotspot the portal tile opens.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, rc::Rc, string::String, string::ToString};
use barracuda_app_manager_plugin::{HotspotFuture, NetworkSource, NetworkState, SystemNetwork};
use barracuda_platform::{AccessPointState, StationState};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginRegisterContext, PluginResult};
use barracuda_wifi_plugin::{SETUP_AP_ADDRESS, SETUP_AP_PASSWORD, SETUP_AP_SSID, WifiControl};

/// Hands the system UI the `wifi` Plugin's link and setup hotspot.
#[barracuda_plugin::macros::plugin]
pub struct SystemWifiPlugin;

impl SystemWifiPlugin {
    /// Creates the unregistered source.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for SystemWifiPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let network = context.require::<SystemNetwork>("app-manager")?;
        let wifi = context.require::<WifiControl>("wifi")?;
        network.show(Rc::new(Wifi(wifi)));
        Ok(())
    }
}

/// Wi-Fi, as the system UI shows it.
struct Wifi(Rc<WifiControl>);

impl NetworkSource for Wifi {
    fn state(&self) -> NetworkState {
        state(self.0.status().station())
    }

    fn hotspot_open(&self) -> bool {
        matches!(
            self.0.status().access_point(),
            AccessPointState::Started { .. } | AccessPointState::Starting
        )
    }

    fn hotspot_hint(&self) -> String {
        format!("连接热点“{SETUP_AP_SSID}”，打开 http://{SETUP_AP_ADDRESS}/")
    }

    fn open_hotspot(&self) -> HotspotFuture<'_> {
        Box::pin(async move {
            self.0
                .start_access_point(SETUP_AP_SSID, SETUP_AP_PASSWORD)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close_hotspot(&self) -> HotspotFuture<'_> {
        Box::pin(async move {
            self.0
                .stop_access_point()
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// The station's link, in the system UI's terms.
fn state(station: &StationState) -> NetworkState {
    match station {
        StationState::Connected { ssid } => NetworkState::Connected { name: ssid.clone() },
        StationState::Connecting => NetworkState::Connecting,
        StationState::Disconnected => NetworkState::Offline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_station_becomes_the_system_uis_link() {
        assert_eq!(
            state(&StationState::Connected {
                ssid: Some(String::from("HomeNet")),
            }),
            NetworkState::Connected {
                name: Some(String::from("HomeNet")),
            }
        );
        assert_eq!(state(&StationState::Connecting), NetworkState::Connecting);
        assert_eq!(state(&StationState::Disconnected), NetworkState::Offline);
    }
}
