//! Power-up, reset, and backlight behavior over a mock PMIC and expander bus.

#![allow(clippy::expect_used, clippy::panic)]

use core::convert::Infallible;
use std::{boxed::Box, vec::Vec};

use barracuda_m5stack_pmic_display::{
    BacklightRail, ILI9342CRgb565, M5StackPmicDisplayImplementation, MipiDbiDisplayConfig,
    MipiDbiDriverConfig, PmicDisplayBindings, PmicDisplayConfig, PmicDisplayInitError, PowerLayout,
    ST7735s, Size,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    display::{Display, DisplayFeatures, DisplayOrientation, DisplayPower, PixelFormat},
};
use embassy_futures::block_on;
use embedded_hal::{
    delay::DelayNs,
    digital::{ErrorType as PinErrorType, OutputPin},
    i2c::{ErrorType as I2cErrorType, I2c, Operation as I2cOperation},
    spi::{ErrorType as SpiErrorType, Operation as SpiOperation, SpiDevice},
};

const PMIC: u8 = 0x34;
const EXPANDER: u8 = 0x58;

/// Register files of the PMIC at 34h and, when fitted, the AW9523B at 58h.
struct Bus {
    pmic: [u8; 0x100],
    expander: Option<[u8; 0x80]>,
    writes: Vec<(u8, u8, u8)>,
}

impl Bus {
    fn with_pmic_id(id: u8) -> Self {
        let mut pmic = [0; 0x100];
        pmic[0x03] = id;
        Self {
            pmic,
            expander: None,
            writes: Vec::new(),
        }
    }

    fn with_expander(mut self) -> Self {
        let mut expander = [0; 0x80];
        expander[0x10] = 0x23;
        expander[0x12] = 0xff;
        expander[0x13] = 0xff;
        expander[0x05] = 0xff;
        self.expander = Some(expander);
        self
    }

    fn register(&mut self, address: u8, register: u8) -> &mut u8 {
        match address {
            PMIC => &mut self.pmic[usize::from(register)],
            EXPANDER => {
                &mut self.expander.as_mut().expect("expander fitted")[usize::from(register)]
            }
            _ => panic!("unexpected I2C address {address:#04x}"),
        }
    }

    fn writes_to(&self, address: u8, register: u8) -> Vec<u8> {
        self.writes
            .iter()
            .filter(|(a, r, _)| *a == address && *r == register)
            .map(|(_, _, value)| *value)
            .collect()
    }
}

impl I2cErrorType for Bus {
    type Error = Infallible;
}

impl I2c for Bus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        let mut register = None;
        for operation in operations {
            match operation {
                I2cOperation::Write([reg]) => register = Some(*reg),
                I2cOperation::Write([reg, value]) => {
                    *self.register(address, *reg) = *value;
                    self.writes.push((address, *reg, *value));
                }
                I2cOperation::Read([value]) => {
                    *value = *self.register(address, register.expect("register selected"));
                }
                _ => panic!("unexpected I2C operation"),
            }
        }
        Ok(())
    }
}

/// A shared view so the test can inspect the bus after the display took it.
struct SharedBus(&'static core::cell::RefCell<Bus>);

impl I2cErrorType for SharedBus {
    type Error = Infallible;
}

impl I2c for SharedBus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        self.0.borrow_mut().transaction(address, operations)
    }
}

struct Spi;

impl SpiErrorType for Spi {
    type Error = Infallible;
}

impl SpiDevice for Spi {
    fn transaction(&mut self, _operations: &mut [SpiOperation<'_, u8>]) -> Result<(), Infallible> {
        Ok(())
    }
}

#[derive(Default)]
struct Pin {
    pulses: u32,
}

impl PinErrorType for Pin {
    type Error = Infallible;
}

impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        self.pulses += 1;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}

struct Delay;

impl DelayNs for Delay {
    fn delay_ns(&mut self, _ns: u32) {}
}

type Implementation<M> = M5StackPmicDisplayImplementation<Spi, Pin, M, Pin, SharedBus, Delay, 64>;

fn bus(bus: Bus) -> &'static core::cell::RefCell<Bus> {
    Box::leak(Box::new(core::cell::RefCell::new(bus)))
}

fn buffer() -> &'static mut [u8; 64] {
    Box::leak(Box::new([0; 64]))
}

fn core2_config() -> PmicDisplayConfig<ILI9342CRgb565> {
    PmicDisplayConfig::new(
        PowerLayout::Core2,
        MipiDbiDriverConfig::new(
            ILI9342CRgb565,
            MipiDbiDisplayConfig::new(
                Size::new(320, 240),
                PixelFormat::Rgb565,
                DisplayOrientation::Deg0,
            )
            .with_inverted_colors(true),
        ),
    )
}

#[test]
fn backlight_curves_match_m5gfx() {
    // Full brightness reaches the top of each curve, the lowest non-zero
    // level the bottom M5GFX keeps visible.
    assert_eq!(BacklightRail::Axp192Dcdc3.millivolts(255), 3275);
    assert_eq!(BacklightRail::Axp192Dcdc3.millivolts(1), 2500);
    assert_eq!(BacklightRail::Axp192Ldo2.millivolts(255), 3300);
    assert_eq!(BacklightRail::Axp192Ldo2.millivolts(1), 2300);
    assert_eq!(BacklightRail::Axp192Ldo3.millivolts(255), 3300);
    assert_eq!(BacklightRail::Axp192Ldo3.millivolts(1), 2500);
    assert_eq!(BacklightRail::Axp2101Bldo1.millivolts(255), 3300);
    assert_eq!(BacklightRail::Axp2101Dldo1.millivolts(1), 2500);
    for level in 1..=u8::MAX {
        let dcdc3 = BacklightRail::Axp192Dcdc3.millivolts(level);
        assert_eq!(dcdc3 % 25, 0);
        for rail in [
            BacklightRail::Axp192Ldo2,
            BacklightRail::Axp192Ldo3,
            BacklightRail::Axp2101Bldo1,
            BacklightRail::Axp2101Dldo1,
        ] {
            let millivolts = rail.millivolts(level);
            assert_eq!(millivolts % 100, 0);
            assert!((2300..=3300).contains(&millivolts));
        }
    }
}

#[test]
fn core2_with_axp192_powers_resets_and_dims_through_the_pmic() {
    let shared = bus(Bus::with_pmic_id(0x03));
    let mut display = block_on(Implementation::<ILI9342CRgb565>::initialize(
        PmicDisplayBindings::new(
            Spi,
            Pin::default(),
            None,
            SharedBus(shared),
            Delay,
            buffer(),
        ),
        core2_config(),
    ))
    .expect("display initializes");

    {
        let bus = shared.borrow();
        // LDO2 at 3.3 V (code 15 in the high nibble) and enabled.
        assert_eq!(bus.pmic[0x28] & 0xf0, 0xf0);
        assert_eq!(bus.pmic[0x12] & 0x04, 0x04);
        // GPIO4 is an open-drain output pulsed low, then released.
        assert_eq!(bus.pmic[0x95] & 0x8c, 0x84);
        assert_eq!(bus.writes_to(PMIC, 0x96), [0x00, 0x02]);
        // DC-DC3 backlight lit at full brightness: 3275 mV is code 103.
        assert_eq!(bus.pmic[0x27], 103);
        assert_eq!(bus.pmic[0x12] & 0x02, 0x02);
    }
    assert!(
        display
            .descriptor()
            .features()
            .contains(DisplayFeatures::BRIGHTNESS)
    );

    block_on(display.set_brightness(0)).expect("dark");
    assert_eq!(shared.borrow().pmic[0x12] & 0x02, 0);
    block_on(display.set_brightness(8)).expect("dim");
    assert_eq!(shared.borrow().pmic[0x27], 73);
    assert_eq!(shared.borrow().pmic[0x12] & 0x02, 0x02);
    block_on(display.set_power(DisplayPower::Sleep)).expect("sleep");
    assert_eq!(shared.borrow().pmic[0x12] & 0x02, 0);
    block_on(display.set_power(DisplayPower::On)).expect("wake");
    assert_eq!(shared.borrow().pmic[0x27], 73);
    assert_eq!(shared.borrow().pmic[0x12] & 0x02, 0x02);
}

#[test]
fn core2_with_axp2101_uses_aldo4_aldo2_and_bldo1() {
    let shared = bus(Bus::with_pmic_id(0x4a));
    let _display = block_on(Implementation::<ILI9342CRgb565>::initialize(
        PmicDisplayBindings::new(
            Spi,
            Pin::default(),
            None,
            SharedBus(shared),
            Delay,
            buffer(),
        ),
        core2_config(),
    ))
    .expect("display initializes");
    let bus = shared.borrow();
    // ALDO4 at 3.3 V; BLDO1 at 3.3 V for full brightness.
    assert_eq!(bus.pmic[0x95], 28);
    assert_eq!(bus.pmic[0x96], 28);
    // REG90H: BLDO1 off, ALDO4 on, ALDO2 pulsed off then on, BLDO1 on.
    assert_eq!(bus.writes_to(PMIC, 0x90), [0x00, 0x08, 0x08, 0x0a, 0x1a]);
}

#[test]
fn cores3_resets_through_the_expander_and_lights_dldo1() {
    let shared = bus(Bus::with_pmic_id(0x4a).with_expander());
    let config = PmicDisplayConfig::new(
        PowerLayout::CoreS3,
        MipiDbiDriverConfig::new(
            ILI9342CRgb565,
            MipiDbiDisplayConfig::new(
                Size::new(320, 240),
                PixelFormat::Rgb565,
                DisplayOrientation::Deg0,
            ),
        ),
    );
    let _display = block_on(Implementation::<ILI9342CRgb565>::initialize(
        PmicDisplayBindings::new(
            Spi,
            Pin::default(),
            None,
            SharedBus(shared),
            Delay,
            buffer(),
        ),
        config,
    ))
    .expect("display initializes");
    let bus = shared.borrow();
    let expander = bus.expander.expect("expander");
    // P1_1 is a GPIO output, pulsed low and left high.
    assert_eq!(expander[0x05] & 0x02, 0);
    assert_eq!(bus.writes_to(EXPANDER, 0x03), [0x00, 0x02]);
    // DLDO1 (REG90H bit 7) lit at 3.3 V.
    assert_eq!(bus.pmic[0x99], 28);
    assert_eq!(bus.pmic[0x90] & 0x80, 0x80);
}

#[test]
fn stickc_powers_ldo3_and_leaves_reset_to_the_gpio() {
    let shared = bus(Bus::with_pmic_id(0x03));
    let config = PmicDisplayConfig::new(
        PowerLayout::StickC,
        MipiDbiDriverConfig::new(
            ST7735s,
            MipiDbiDisplayConfig::new(
                Size::new(80, 160),
                PixelFormat::Rgb565,
                DisplayOrientation::Deg180,
            )
            .with_offset(26, 1)
            .with_inverted_colors(true),
        ),
    );
    let _display = block_on(M5StackPmicDisplayImplementation::<
        Spi,
        Pin,
        ST7735s,
        Pin,
        SharedBus,
        Delay,
        64,
    >::initialize(
        PmicDisplayBindings::new(
            Spi,
            Pin::default(),
            Some(Pin::default()),
            SharedBus(shared),
            Delay,
            buffer(),
        ),
        config,
    ))
    .expect("display initializes");
    let bus = shared.borrow();
    // LDO3 at 3.0 V (code 12) for the LCD, LDO2 at 3.3 V for the backlight.
    assert_eq!(bus.pmic[0x28], 0xfc);
    assert_eq!(bus.pmic[0x12] & 0x0c, 0x0c);
    // The AXP192 GPIOs are left alone.
    assert!(bus.writes_to(PMIC, 0x95).is_empty());
    assert!(bus.writes_to(PMIC, 0x96).is_empty());
}

#[test]
fn rejects_mismatched_pmic_and_reset_wiring() {
    let shared = bus(Bus::with_pmic_id(0x4a));
    let tough = PmicDisplayConfig::new(PowerLayout::Tough, core2_config_panel());
    assert!(matches!(
        block_on(Implementation::<ILI9342CRgb565>::initialize(
            PmicDisplayBindings::new(
                Spi,
                Pin::default(),
                None,
                SharedBus(shared),
                Delay,
                buffer()
            ),
            tough,
        )),
        Err(PmicDisplayInitError::UnsupportedPmic)
    ));

    let shared = bus(Bus::with_pmic_id(0x03));
    assert!(matches!(
        block_on(Implementation::<ILI9342CRgb565>::initialize(
            PmicDisplayBindings::new(
                Spi,
                Pin::default(),
                Some(Pin::default()),
                SharedBus(shared),
                Delay,
                buffer(),
            ),
            core2_config(),
        )),
        Err(PmicDisplayInitError::ResetWiring)
    ));
    assert!(shared.borrow().writes.is_empty());

    let cores3 = PmicDisplayConfig::new(PowerLayout::CoreS3, core2_config_panel());
    let mut without_expander_id = Bus::with_pmic_id(0x4a).with_expander();
    if let Some(registers) = without_expander_id.expander.as_mut() {
        registers[0x10] = 0;
    }
    let missing = bus(without_expander_id);
    assert!(matches!(
        block_on(Implementation::<ILI9342CRgb565>::initialize(
            PmicDisplayBindings::new(
                Spi,
                Pin::default(),
                None,
                SharedBus(missing),
                Delay,
                buffer()
            ),
            cores3,
        )),
        Err(PmicDisplayInitError::MissingExpander)
    ));
}

fn core2_config_panel() -> MipiDbiDriverConfig<ILI9342CRgb565> {
    MipiDbiDriverConfig::new(
        ILI9342CRgb565,
        MipiDbiDisplayConfig::new(
            Size::new(320, 240),
            PixelFormat::Rgb565,
            DisplayOrientation::Deg0,
        ),
    )
}
