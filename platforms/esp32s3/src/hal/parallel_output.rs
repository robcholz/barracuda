//! ESP32-S3 eight-bit Intel 8080-style parallel output on the LCD_CAM
//! controller.
//!
//! One implementation serves both kinds of consumer the binding has: a MIPI
//! DBI panel controller, which the Board wires with a data/command line, and a
//! directly driven e-paper source driver, which takes only data (M5GFX
//! `Bus_EPD` drives it through the same `esp_lcd` i80 bus, its source clock on
//! the write strobe and its source start pulse on the chip select).

use barracuda_peripheral::display::ParallelOutput;
use esp_hal::{
    dma::{DmaBufError, DmaError, DmaTxBuf},
    gpio::interconnect::PeripheralOutput,
    lcd_cam::{
        lcd::i8080::{Command, Config, ConfigError as VendorConfigError, I8080},
        LcdCam, LcdDmaTxChannel,
    },
    peripherals::LCD_CAM,
    time::Rate,
    Blocking,
};

/// ESP32-S3 parallel-output construction failure.
#[derive(Debug)]
pub enum ParallelOutputConfigError {
    /// The Board-selected DMA storage cannot be used by the controller.
    DmaBuffer(DmaBufError),
    /// The requested write-strobe clock is unsupported.
    Peripheral(VendorConfigError),
}

impl core::fmt::Display for ParallelOutputConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DmaBuffer(_) => formatter.write_str("invalid parallel-output DMA buffer"),
            Self::Peripheral(_) => {
                formatter.write_str("invalid ESP32-S3 parallel-output configuration")
            }
        }
    }
}

impl core::error::Error for ParallelOutputConfigError {}

/// ESP32-S3 parallel-output transfer failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParallelOutputError {
    /// The DMA engine rejected or aborted a transfer.
    Dma(DmaError),
    /// An earlier failed transfer lost the controller.
    Unavailable,
    /// A command was written to a bus the Board wires without a
    /// data/command line.
    NoDataCommandLine,
}

impl core::fmt::Display for ParallelOutputError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Dma(_) => formatter.write_str("parallel-output DMA transfer failed"),
            Self::Unavailable => formatter.write_str("parallel output is unavailable"),
            Self::NoDataCommandLine => {
                formatter.write_str("parallel output has no data/command line")
            }
        }
    }
}

impl core::error::Error for ParallelOutputError {}

/// Selected eight-bit LCD_CAM output with its DMA staging buffer.
pub struct ParallelOutputBus {
    bus: Option<I8080<'static, Blocking>>,
    buffer: Option<DmaTxBuf>,
    data_command: bool,
}

impl ParallelOutputBus {
    /// Sends `data` after `command`, in as many DMA transfers as the staging
    /// buffer requires; only the first carries the command phase.
    fn send(&mut self, command: Option<u8>, data: &[u8]) -> Result<(), ParallelOutputError> {
        let mut command = command.map_or(Command::None, Command::One);
        let mut chunks = data.chunks(self.capacity().max(1));
        let mut chunk = chunks.next().unwrap_or(&[]);
        loop {
            self.transfer(command, chunk)?;
            command = Command::None;
            match chunks.next() {
                Some(next) => chunk = next,
                None => return Ok(()),
            }
        }
    }

    fn transfer(&mut self, command: Command<u8>, chunk: &[u8]) -> Result<(), ParallelOutputError> {
        let (Some(bus), Some(mut buffer)) = (self.bus.take(), self.buffer.take()) else {
            return Err(ParallelOutputError::Unavailable);
        };
        buffer.fill(chunk);
        match bus.send(command, 0, buffer) {
            Ok(transfer) => {
                let (result, bus, buffer) = transfer.wait();
                self.bus = Some(bus);
                self.buffer = Some(buffer);
                result.map_err(ParallelOutputError::Dma)
            }
            Err((error, bus, buffer)) => {
                self.bus = Some(bus);
                self.buffer = Some(buffer);
                Err(ParallelOutputError::Dma(error))
            }
        }
    }
}

impl ParallelOutput for ParallelOutputBus {
    type Error = ParallelOutputError;

    fn capacity(&self) -> usize {
        self.buffer.as_ref().map_or(0, DmaTxBuf::capacity)
    }

    fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error> {
        if !self.data_command {
            return Err(ParallelOutputError::NoDataCommandLine);
        }
        self.send(Some(command), data)
    }

    fn write_data(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        if data.is_empty() {
            return Ok(());
        }
        self.send(None, data)
    }
}

/// Constructs the ESP32-S3 parallel output of a panel controller that tells
/// commands from data. Data/command idles high, is low for the command phase,
/// and high for data, as Espressif's `esp_lcd` i80 bus drives it.
///
/// # Errors
///
/// Returns [`ParallelOutputConfigError`] when the clock is unsupported.
#[allow(clippy::too_many_arguments)]
pub fn parallel_output(
    controller: LCD_CAM<'static>,
    dma: impl LcdDmaTxChannel<'static>,
    wr: impl PeripheralOutput<'static>,
    dc: impl PeripheralOutput<'static>,
    chip_select: impl PeripheralOutput<'static>,
    data0: impl PeripheralOutput<'static>,
    data1: impl PeripheralOutput<'static>,
    data2: impl PeripheralOutput<'static>,
    data3: impl PeripheralOutput<'static>,
    data4: impl PeripheralOutput<'static>,
    data5: impl PeripheralOutput<'static>,
    data6: impl PeripheralOutput<'static>,
    data7: impl PeripheralOutput<'static>,
    frequency_hz: u32,
    buffer: DmaTxBuf,
) -> Result<ParallelOutputBus, ParallelOutputConfigError> {
    let bus = I8080::new(LcdCam::new(controller).lcd, dma, config(frequency_hz))
        .map_err(ParallelOutputConfigError::Peripheral)?
        .with_wrx(wr)
        .with_dc(dc)
        .with_cs(chip_select)
        .with_data0(data0)
        .with_data1(data1)
        .with_data2(data2)
        .with_data3(data3)
        .with_data4(data4)
        .with_data5(data5)
        .with_data6(data6)
        .with_data7(data7);
    Ok(ParallelOutputBus {
        bus: Some(bus),
        buffer: Some(buffer),
        data_command: true,
    })
}

/// Constructs the ESP32-S3 parallel output of a Board that wires no
/// data/command line, such as a directly driven e-paper source driver: every
/// byte is data.
///
/// # Errors
///
/// Returns [`ParallelOutputConfigError`] when the clock is unsupported.
#[allow(clippy::too_many_arguments)]
pub fn parallel_output_data_only(
    controller: LCD_CAM<'static>,
    dma: impl LcdDmaTxChannel<'static>,
    wr: impl PeripheralOutput<'static>,
    chip_select: impl PeripheralOutput<'static>,
    data0: impl PeripheralOutput<'static>,
    data1: impl PeripheralOutput<'static>,
    data2: impl PeripheralOutput<'static>,
    data3: impl PeripheralOutput<'static>,
    data4: impl PeripheralOutput<'static>,
    data5: impl PeripheralOutput<'static>,
    data6: impl PeripheralOutput<'static>,
    data7: impl PeripheralOutput<'static>,
    frequency_hz: u32,
    buffer: DmaTxBuf,
) -> Result<ParallelOutputBus, ParallelOutputConfigError> {
    let bus = I8080::new(LcdCam::new(controller).lcd, dma, config(frequency_hz))
        .map_err(ParallelOutputConfigError::Peripheral)?
        .with_wrx(wr)
        .with_cs(chip_select)
        .with_data0(data0)
        .with_data1(data1)
        .with_data2(data2)
        .with_data3(data3)
        .with_data4(data4)
        .with_data5(data5)
        .with_data6(data6)
        .with_data7(data7);
    Ok(ParallelOutputBus {
        bus: Some(bus),
        buffer: Some(buffer),
        data_command: false,
    })
}

/// The write strobe idles low (the vendor default, and M5GFX `Bus_EPD`'s
/// `pclk_idle_low`); data/command follows `esp_lcd`'s i80 levels.
fn config(frequency_hz: u32) -> Config {
    Config::default()
        .with_frequency(Rate::from_hz(frequency_hz))
        .with_cd_idle_edge(true)
        .with_cd_cmd_edge(false)
        .with_cd_dummy_edge(false)
        .with_cd_data_edge(true)
}
