//! Wi-Fi control behavior shared by Host and embedded Platforms.
#![allow(clippy::expect_used)]

use barracuda_platform::{HostWifiDevice, WifiCapabilities};
use barracuda_platform_test::never_embassy_stack;
use barracuda_wifi_plugin::{WifiControl, WifiStatus};
use futures_lite::future::block_on;

#[test]
fn host_control_reports_connected_platform_managed_network() {
    let control = WifiControl::new(HostWifiDevice::new(never_embassy_stack()));

    assert_eq!(control.capabilities(), WifiCapabilities::host_managed());
    assert_eq!(control.status(), WifiStatus::platform_managed());
}

#[test]
fn host_control_treats_ap_and_station_commands_as_noops() {
    let control = WifiControl::new(HostWifiDevice::new(never_embassy_stack()));

    block_on(async {
        control
            .start_access_point("setup", "secret")
            .await
            .expect("AP noop");
        control.stop_access_point().await.expect("AP noop");
        control
            .connect_station("home", "secret")
            .await
            .expect("station noop");
        control.disconnect_station().await.expect("station noop");
    });

    assert_eq!(control.status(), WifiStatus::platform_managed());
}
