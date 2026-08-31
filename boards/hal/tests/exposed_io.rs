//! Ownership and naming behavior of explicitly exposed Board I/O.

#![allow(clippy::expect_used)]

use barracuda_board_hal::{ExposedIo, NamedResources, NoExposedIo, ResourceSet};

#[test]
fn named_resources_keep_concrete_values_and_support_runtime_names() {
    let mut resources = NamedResources::new([("status", 10_u8), ("button", 11_u8)]);

    assert!(resources.contains("status"));
    assert!(!resources.contains("missing"));
    *resources.get_mut("button").expect("named resource") = 12;
    assert_eq!(resources.get("button"), Some(&12));
}

#[test]
fn taking_an_exposed_resource_is_move_only() {
    struct Io {
        gpio: Option<NamedResources<u8, 1>>,
    }

    impl ExposedIo for Io {
        type Gpio = NamedResources<u8, 1>;
        type I2c = NamedResources<u8, 0>;
        type Spi = NamedResources<u8, 0>;

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

    let mut io = Io {
        gpio: Some(NamedResources::new([("status", 10)])),
    };
    let mut gpio = io.take_gpio().expect("first owner takes GPIO set");
    assert_eq!(gpio.get_mut("status").copied(), Some(10));
    assert!(io.take_gpio().is_none());
}

#[test]
fn empty_io_uses_typed_empty_resource_sets() {
    let mut io = NoExposedIo;

    assert!(io.take_gpio().is_none());
    assert!(io.take_i2c().is_none());
    assert!(io.take_spi().is_none());
}
