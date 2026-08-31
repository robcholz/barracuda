//! Dynamic service handles used at the scripting ownership boundary.

#![allow(clippy::expect_used)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

extern crate alloc;

use barracuda_board_hal::{
    GpioService, HardwareServices, InputConfig, IntoHardwareServices, IoServiceResult, NoExposedIo,
    OutputConfig, ServiceFuture,
};

struct TestGpio;

impl GpioService for TestGpio {
    fn contains(&self, name: &str) -> bool {
        name == "user-control"
    }

    fn configure_input(&self, _name: String, _config: InputConfig) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn configure_output(&self, _name: String, _config: OutputConfig) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn disable(&self, _name: String) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn read(&self, name: String) -> ServiceFuture<'_, bool> {
        Box::pin(async move { Ok(name == "user-control") })
    }

    fn write(&self, _name: String, _high: bool) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn assert_result_type(_result: IoServiceResult<bool>) {}

#[test]
fn hardware_services_retain_typed_optional_handles() {
    let services = HardwareServices::new().with_gpio(Arc::new(TestGpio));
    let gpio = services.gpio().expect("GPIO service");

    assert!(gpio.contains("user-control"));
    assert!(!gpio.contains("missing"));
    assert_result_type(Ok(true));
}

#[test]
fn empty_board_io_converts_to_no_hardware_services() {
    let services = NoExposedIo.into_hardware_services();

    assert!(services.gpio().is_none());
    assert!(services.i2c().is_none());
    assert!(services.spi().is_none());
}
