//! System construction handles flow through the shared Plugin context.

use std::sync::Arc;

use barracuda_board_hal::HardwareServices;
use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{ClientFactory, PluginContext};

#[test]
fn plugin_context_carries_board_hardware_services() {
    let stack = never_embassy_stack();
    let hardware = HardwareServices::new();
    let context =
        PluginContext::new(stack, ClientFactory::plaintext(stack)).with_hardware_services(hardware);

    assert!(context.hardware_services.gpio().is_none());
    assert!(context.hardware_services.i2c().is_none());
    assert!(context.hardware_services.spi().is_none());

    let _: Option<Arc<dyn barracuda_board_hal::GpioService>> = context.hardware_services.gpio();
}
