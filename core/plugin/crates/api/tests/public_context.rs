//! Public construction context API tests.

use barracuda_platform::{HostWifiDevice, StationState, WifiCapabilities, WifiDevice};
use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{
    BoardInfo, Entropy, Hardware, PlatformInfo, PlatformWifi, PluginContext, TargetIdentity,
};

#[test]
fn construction_resources_are_public_fields() {
    let stack = never_embassy_stack();
    let identity = TargetIdentity::new(
        PlatformInfo::new("test", "test", "test-arch", "hosted"),
        BoardInfo::new("test-board", Hardware::new("test-chip")),
    );
    let context = PluginContext::new(identity, stack);

    let _target_identity = context.target_identity;
    let _ip_stack = context.ip_stack;
    assert!(context.entropy.fill(&mut [0; 4]).is_err());
    assert!(context.wifi.is_none());
    let _hal = context.hal;
}

#[test]
fn platform_wifi_forwards_the_erased_device() {
    let wifi = PlatformWifi::new(HostWifiDevice::new(never_embassy_stack()));

    assert_eq!(wifi.capabilities(), WifiCapabilities::host_managed());
    assert_eq!(wifi.station_state(), StationState::Connected { ssid: None });
}
