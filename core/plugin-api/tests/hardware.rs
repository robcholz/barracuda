//! The shared Plugin context carries the complete HAL without splitting it.

use std::boxed::Box;

use barracuda_board_hal::BoardHalResources;
use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{
    ClientFactory, LuaGpioHardware, LuaHardwareFuture, LuaIo, PluginContext,
};

struct TestGpio;

struct TestIo {
    gpio: Option<Box<dyn LuaGpioHardware>>,
}

impl LuaIo for TestIo {
    fn take_gpio(&mut self) -> Option<Box<dyn LuaGpioHardware>> {
        self.gpio.take()
    }

    fn take_i2c(&mut self) -> Option<Box<dyn barracuda_plugin_api::LuaI2cHardware>> {
        None
    }

    fn take_spi(&mut self) -> Option<Box<dyn barracuda_plugin_api::LuaSpiHardware>> {
        None
    }
}

impl LuaGpioHardware for TestGpio {
    fn contains(&self, name: &str) -> bool {
        name == "user-control"
    }

    fn configure_input(
        &mut self,
        _name: String,
        _config: barracuda_board_hal::InputConfig,
    ) -> LuaHardwareFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn configure_output(
        &mut self,
        _name: String,
        _config: barracuda_board_hal::OutputConfig,
    ) -> LuaHardwareFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn disable(&mut self, _name: String) -> LuaHardwareFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn read(&mut self, name: String) -> LuaHardwareFuture<'_, bool> {
        Box::pin(async move { Ok(name == "user-control") })
    }

    fn write(&mut self, _name: String, _high: bool) -> LuaHardwareFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn plugin_context_carries_the_complete_hal_and_its_move_only_io() {
    let stack = never_embassy_stack();
    let hal = BoardHalResources::new(
        7_u8,
        TestIo {
            gpio: Some(Box::new(TestGpio)),
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
