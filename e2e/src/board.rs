//! Mock Board HAL owned by the e2e composition.

use core::convert::Infallible;

use barracuda_board_hal::{
    BoardHal, BoardHalInitResult, BoardHalResources, ExposedIo, NamedResources,
};
use barracuda_indicator_led::{ActiveLevel, IndicatorLed};
use embassy_executor::Spawner;

use crate::mock_drivers::{MockGpio, MockI2c, MockSpi};

/// Built-in semantic devices supplied by the e2e Board.
pub struct E2eBuiltins {
    #[allow(dead_code)]
    pub(crate) status_indicator: IndicatorLed<MockGpio>,
}

/// Named hardware capabilities supplied by the e2e Board.
pub struct E2eIo {
    gpio: Option<NamedResources<MockGpio, 1>>,
    i2c: Option<NamedResources<MockI2c, 1>>,
    spi: Option<NamedResources<MockSpi, 1>>,
}

impl ExposedIo for E2eIo {
    type Gpio = NamedResources<MockGpio, 1>;
    type I2c = NamedResources<MockI2c, 1>;
    type Spi = NamedResources<MockSpi, 1>;

    fn take_gpio(&mut self) -> Option<Self::Gpio> {
        self.gpio.take()
    }

    fn take_i2c(&mut self) -> Option<Self::I2c> {
        self.i2c.take()
    }

    fn take_spi(&mut self) -> Option<Self::Spi> {
        self.spi.take()
    }
}

/// Deterministic mock Board HAL owned by the e2e crate.
pub struct E2eBoardHal;

impl BoardHal for E2eBoardHal {
    type Bindings = ();
    type Resources = BoardHalResources<E2eBuiltins, E2eIo>;
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _bindings: ()) -> BoardHalInitResult<Self> {
        let builtins = E2eBuiltins {
            status_indicator: IndicatorLed::new(MockGpio::new(), ActiveLevel::High),
        };
        let io = E2eIo {
            gpio: Some(NamedResources::new([("user-gpio", MockGpio::new())])),
            i2c: Some(NamedResources::new([("sensor-bus", MockI2c)])),
            spi: Some(NamedResources::new([("display-bus", MockSpi)])),
        };
        Ok(BoardHalResources::new(builtins, io))
    }
}
