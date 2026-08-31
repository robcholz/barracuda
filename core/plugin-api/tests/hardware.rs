//! Move-only Lua hardware values flow through the shared Plugin context.

use std::boxed::Box;

use barracuda_platform_test::never_embassy_stack;
use barracuda_plugin_api::{
    ClientFactory, LuaGpioHardware, LuaHardwareFuture, LuaHardwareResources, PluginContext,
};

struct TestGpio;

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
fn plugin_context_allows_exactly_one_plugin_to_take_each_hardware_value() {
    let stack = never_embassy_stack();
    let hardware = LuaHardwareResources::new().with_gpio(Box::new(TestGpio));
    let context =
        PluginContext::new(stack, ClientFactory::plaintext(stack)).with_lua_hardware(hardware);

    assert!(context
        .take_gpio()
        .is_some_and(|gpio| gpio.contains("user-control")));
    assert!(context.take_gpio().is_none());
}
