//! Portable Wi-Fi capability contract.

use barracuda_platform::{AccessPointConfiguration, StationConfiguration, WifiCapabilities};

#[test]
fn host_managed_wifi_has_no_configuration_controls() {
    let capabilities = WifiCapabilities::host_managed();

    assert!(!capabilities.access_point);
    assert!(!capabilities.scanning);
    assert!(!capabilities.station_configuration);
}

#[test]
fn station_and_access_point_configuration_preserve_credentials() {
    let station = StationConfiguration::new("home", "station-secret");
    let access_point = AccessPointConfiguration::new("barracuda-setup", "setup-secret");

    assert_eq!(station.ssid(), "home");
    assert_eq!(station.password(), "station-secret");
    assert_eq!(access_point.ssid(), "barracuda-setup");
    assert_eq!(access_point.password(), "setup-secret");
}
