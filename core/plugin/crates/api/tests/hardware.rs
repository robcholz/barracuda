//! The shared Plugin context carries the complete HAL without splitting it.

use barracuda_board_hal::{
    BoardHalResources, ExposedIo, NamedResources, ResourceSet, UnavailableI2c, UnavailableSpi,
};
use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{ClientFactory, PluginContext};

struct TestGpio;

struct TestIo {
    gpio: Option<NamedResources<TestGpio, 1>>,
}

impl ExposedIo for TestIo {
    type Gpio = NamedResources<TestGpio, 1>;
    type I2c = NamedResources<UnavailableI2c, 0>;
    type Spi = NamedResources<UnavailableSpi, 0>;

    fn take_gpio(&mut self) -> Option<Self::Gpio> {
        self.gpio.take()
    }

    fn take_i2c(&mut self) -> Option<Self::I2c> {
        None
    }

    fn take_spi(&mut self) -> Option<Self::Spi> {
        None
    }
}

#[test]
fn plugin_context_carries_the_complete_hal_and_its_move_only_io() {
    let stack = never_embassy_stack();
    let hal = BoardHalResources::new(
        7_u8,
        TestIo {
            gpio: Some(NamedResources::new([("user-control", TestGpio)])),
        },
    );
    let mut context = PluginContext::from_hal(stack, ClientFactory::plaintext(stack), hal);

    assert_eq!(context.hal.builtins, 7);
    assert!(context
        .hal
        .io
        .take_gpio()
        .is_some_and(|gpio| gpio.contains("user-control")));
    assert!(context.hal.io.take_gpio().is_none());
}
