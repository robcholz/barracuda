//! The shared Plugin context carries the complete HAL without splitting it.

use std::sync::Arc;

use barracuda_board_hal::{BoardResources, ExposedIo};
use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{ClientFactory, PluginContext};

struct TestIo;

impl ExposedIo for TestIo {}

#[test]
fn plugin_context_carries_one_shared_runtime_io_owner() {
    let stack = never_embassy_stack();
    let hal = BoardResources::new(7_u8, TestIo);
    let context = PluginContext::from_hal(stack, ClientFactory::plaintext(stack), hal);

    assert_eq!(context.hal.peripherals, 7);
    let shared = Arc::clone(&context.hal.exposed_io);
    assert!(Arc::ptr_eq(&shared, &context.hal.exposed_io));
}
