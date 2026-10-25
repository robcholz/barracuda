//! Protocol driver for 24Cxx-class I2C serial EEPROMs.
//!
//! The Microchip AT24C family and its second sources (the Fudan FM24C02 on
//! the M5Stack Paper among them) share one protocol:
//!
//! - a read sends the word address, then reads sequentially;
//! - a write sends the word address and at most one page of data, which the
//!   chip then programs in an internal cycle (t<sub>WR</sub>, 5 ms at most on
//!   the AT24C02 and FM24C02) during which it does not acknowledge its
//!   address. A write that runs past the end of its page wraps to the
//!   page's start, so this driver refuses one.
//!
//! Parts up to 2 KiB (24C01 to 24C16) take a one-byte word address and
//! carry the higher address bits, the block, in the low bits of the device
//! address; larger parts (24C32 to 24C512) take a two-byte word address.
//!
//! The driver performs single transactions. Waiting out the write cycle,
//! by polling [`At24c::is_ready`], is the caller's.

#![no_std]

use embedded_hal::i2c::{Error as _, ErrorKind, I2c};

/// Largest page of any supported part, in bytes.
pub const MAX_PAGE_SIZE: usize = 128;

/// One-byte word addresses cover 256 bytes; higher bits select the block.
const BLOCK_SIZE: u32 = 256;

/// A supported 24Cxx density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// 1 Kbit: 128 bytes in 8-byte pages.
    C01,
    /// 2 Kbit: 256 bytes in 8-byte pages (AT24C02, FM24C02).
    C02,
    /// 4 Kbit: 512 bytes in 16-byte pages.
    C04,
    /// 8 Kbit: 1 KiB in 16-byte pages.
    C08,
    /// 16 Kbit: 2 KiB in 16-byte pages.
    C16,
    /// 32 Kbit: 4 KiB in 32-byte pages.
    C32,
    /// 64 Kbit: 8 KiB in 32-byte pages.
    C64,
    /// 128 Kbit: 16 KiB in 64-byte pages.
    C128,
    /// 256 Kbit: 32 KiB in 64-byte pages.
    C256,
    /// 512 Kbit: 64 KiB in 128-byte pages.
    C512,
}

impl Variant {
    /// Returns the number of bytes the part holds.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        match self {
            Self::C01 => 128,
            Self::C02 => 256,
            Self::C04 => 512,
            Self::C08 => 1024,
            Self::C16 => 2048,
            Self::C32 => 4096,
            Self::C64 => 8192,
            Self::C128 => 16_384,
            Self::C256 => 32_768,
            Self::C512 => 65_536,
        }
    }

    /// Returns the bytes one write cycle programs.
    #[must_use]
    pub const fn page_size(self) -> u16 {
        match self {
            Self::C01 | Self::C02 => 8,
            Self::C04 | Self::C08 | Self::C16 => 16,
            Self::C32 | Self::C64 => 32,
            Self::C128 | Self::C256 => 64,
            Self::C512 => 128,
        }
    }

    /// Returns whether the part takes a two-byte word address.
    #[must_use]
    pub const fn two_byte_address(self) -> bool {
        matches!(
            self,
            Self::C32 | Self::C64 | Self::C128 | Self::C256 | Self::C512
        )
    }

    /// Device-address bits a one-byte-address part uses for its block.
    const fn block_mask(self) -> u8 {
        match self {
            Self::C04 => 0b001,
            Self::C08 => 0b011,
            Self::C16 => 0b111,
            _ => 0,
        }
    }
}

/// Transport or argument failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The underlying I2C transaction failed.
    Bus(E),
    /// The range does not lie within the part.
    OutOfRange,
    /// A page write's data crosses a page boundary.
    CrossesPage,
}

/// One 24Cxx EEPROM at a caller-selected seven-bit device address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At24c {
    address: u8,
    variant: Variant,
}

impl At24c {
    /// Creates a driver for `variant` at `address`, the device address with
    /// its block bits clear; `None` when the address is not seven bits or
    /// sets bits the part uses for its block.
    #[must_use]
    pub const fn new(address: u8, variant: Variant) -> Option<Self> {
        if address > 0x7f || address & variant.block_mask() != 0 {
            None
        } else {
            Some(Self { address, variant })
        }
    }

    /// Returns the part's density.
    #[must_use]
    pub const fn variant(self) -> Variant {
        self.variant
    }

    /// Returns the offset just past the page that holds `offset`.
    #[must_use]
    pub const fn page_end(self, offset: u32) -> u32 {
        let page = self.variant.page_size() as u32;
        let into_page = match offset.checked_rem(page) {
            Some(remainder) => remainder,
            None => 0,
        };
        offset.saturating_sub(into_page).saturating_add(page)
    }

    /// Reads `buffer.len()` bytes starting at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OutOfRange`] before any transfer when the range
    /// leaves the part, and [`Error::Bus`] when a transfer fails.
    pub fn read<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        offset: u32,
        buffer: &mut [u8],
    ) -> Result<(), Error<I2C::Error>> {
        self.check_range(offset, buffer.len())?;
        if self.variant.two_byte_address() {
            let word = word_address(offset);
            return i2c
                .write_read(self.address, &word, buffer)
                .map_err(Error::Bus);
        }
        // A one-byte-address part addresses each 256-byte block as its own
        // device; read block by block.
        let mut offset = offset;
        let mut rest = buffer;
        while !rest.is_empty() {
            let block_end = offset
                .saturating_sub(offset.checked_rem(BLOCK_SIZE).unwrap_or(0))
                .saturating_add(BLOCK_SIZE);
            let length = usize::try_from(block_end.saturating_sub(offset))
                .unwrap_or(usize::MAX)
                .min(rest.len());
            let (chunk, remaining) = rest.split_at_mut(length);
            let [_, _, high, low] = offset.to_be_bytes();
            i2c.write_read(self.device_address(high), &[low], chunk)
                .map_err(Error::Bus)?;
            offset = offset.saturating_add(u32::try_from(length).unwrap_or(u32::MAX));
            rest = remaining;
        }
        Ok(())
    }

    /// Writes `data` at `offset`, all within one page, and starts the
    /// part's write cycle. Empty data writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OutOfRange`] or [`Error::CrossesPage`] before any
    /// transfer, and [`Error::Bus`] when the transfer fails.
    pub fn write_page<I2C: I2c>(
        &self,
        i2c: &mut I2C,
        offset: u32,
        data: &[u8],
    ) -> Result<(), Error<I2C::Error>> {
        if data.is_empty() {
            return Ok(());
        }
        self.check_range(offset, data.len())?;
        let room =
            usize::try_from(self.page_end(offset).saturating_sub(offset)).unwrap_or(usize::MAX);
        if data.len() > room {
            return Err(Error::CrossesPage);
        }
        let mut frame = [0u8; MAX_PAGE_SIZE + 2];
        let (device, header) = if self.variant.two_byte_address() {
            let word = word_address(offset);
            frame[..2].copy_from_slice(&word);
            (self.address, 2_usize)
        } else {
            let [_, _, high, low] = offset.to_be_bytes();
            frame[0] = low;
            (self.device_address(high), 1)
        };
        let end = header.saturating_add(data.len());
        let body = frame.get_mut(header..end).ok_or(Error::CrossesPage)?;
        body.copy_from_slice(data);
        let frame = frame.get(..end).ok_or(Error::CrossesPage)?;
        i2c.write(device, frame).map_err(Error::Bus)
    }

    /// Reports whether the part acknowledges its address, which it does not
    /// while it programs a page. The probe is a one-byte current-address
    /// read, which changes no data.
    ///
    /// # Errors
    ///
    /// Returns the bus error when the transfer fails other than by a
    /// missing acknowledge.
    pub fn is_ready<I2C: I2c>(&self, i2c: &mut I2C) -> Result<bool, I2C::Error> {
        let mut probe = [0u8; 1];
        match i2c.read(self.address, &mut probe) {
            Ok(()) => Ok(true),
            Err(error) if matches!(error.kind(), ErrorKind::NoAcknowledge(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn check_range<E>(&self, offset: u32, length: usize) -> Result<(), Error<E>> {
        let end = u32::try_from(length)
            .ok()
            .and_then(|length| offset.checked_add(length));
        match end {
            Some(end) if end <= self.variant.capacity() => Ok(()),
            _ => Err(Error::OutOfRange),
        }
    }

    const fn device_address(self, block: u8) -> u8 {
        self.address | (block & self.variant.block_mask())
    }
}

fn word_address(offset: u32) -> [u8; 2] {
    let [_, _, high, low] = offset.to_be_bytes();
    [high, low]
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

    use super::{At24c, Error, Variant};

    #[derive(Debug, PartialEq, Eq)]
    struct Nack;

    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> ErrorKind {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        }
    }

    /// A 24Cxx model: one memory, a word-address pointer, page wrap, and a
    /// write cycle that ignores the next `busy_polls` address phases.
    struct Memory {
        base: u8,
        variant: Variant,
        cells: Vec<u8>,
        pointer: usize,
        busy_polls: u8,
        cycle_polls: u8,
        writes: Vec<(u8, Vec<u8>)>,
    }

    impl Memory {
        fn new(base: u8, variant: Variant, cycle_polls: u8) -> Self {
            Self {
                base,
                variant,
                cells: (0..variant.capacity()).map(|i| i as u8).collect(),
                pointer: 0,
                busy_polls: 0,
                cycle_polls,
                writes: Vec::new(),
            }
        }

        /// The block a device address selects, or `None` for another device.
        fn block(&self, address: u8) -> Option<usize> {
            let mask = self.variant.block_mask();
            (address & !mask == self.base).then(|| usize::from(address & mask))
        }
    }

    impl ErrorType for Memory {
        type Error = Nack;
    }

    impl I2c for Memory {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let block = self.block(address).ok_or(Nack)?;
            if self.busy_polls > 0 {
                self.busy_polls -= 1;
                return Err(Nack);
            }
            let capacity = self.cells.len();
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.writes.push((address, bytes.to_vec()));
                        let header = if self.variant.two_byte_address() {
                            2
                        } else {
                            1
                        };
                        let (word, data) = bytes.split_at(header.min(bytes.len()));
                        self.pointer = if self.variant.two_byte_address() {
                            usize::from(word[0]) << 8 | usize::from(word[1])
                        } else {
                            block << 8 | usize::from(word[0])
                        } % capacity;
                        if !data.is_empty() {
                            let page = usize::from(self.variant.page_size());
                            let start = self.pointer - self.pointer % page;
                            for (index, value) in data.iter().enumerate() {
                                let cell = start + (self.pointer - start + index) % page;
                                self.cells[cell] = *value;
                            }
                            self.busy_polls = self.cycle_polls;
                        }
                    }
                    Operation::Read(buffer) => {
                        for value in buffer.iter_mut() {
                            *value = self.cells[self.pointer];
                            self.pointer = (self.pointer + 1) % capacity;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn rejects_block_bits_in_the_device_address() {
        assert!(At24c::new(0x50, Variant::C02).is_some());
        assert!(At24c::new(0x51, Variant::C02).is_some());
        assert!(At24c::new(0x51, Variant::C04).is_none());
        assert!(At24c::new(0x54, Variant::C16).is_none());
        assert!(At24c::new(0x80, Variant::C02).is_none());
    }

    #[test]
    fn reads_and_writes_a_24c02_page() {
        let mut memory = Memory::new(0x50, Variant::C02, 0);
        let eeprom = At24c::new(0x50, Variant::C02).expect("address");
        eeprom
            .write_page(&mut memory, 0x10, &[0xaa, 0xbb, 0xcc])
            .expect("write");
        assert_eq!(memory.writes, [(0x50, std::vec![0x10, 0xaa, 0xbb, 0xcc])]);
        let mut buffer = [0u8; 5];
        eeprom.read(&mut memory, 0x0f, &mut buffer).expect("read");
        assert_eq!(buffer, [0x0f, 0xaa, 0xbb, 0xcc, 0x13]);
    }

    #[test]
    fn refuses_a_write_that_would_wrap_its_page() {
        let mut memory = Memory::new(0x50, Variant::C02, 0);
        let eeprom = At24c::new(0x50, Variant::C02).expect("address");
        assert_eq!(eeprom.page_end(0x0e), 0x10);
        assert_eq!(
            eeprom.write_page(&mut memory, 0x0e, &[1, 2, 3]),
            Err(Error::CrossesPage)
        );
        assert!(memory.writes.is_empty());
        eeprom.write_page(&mut memory, 0x0e, &[1, 2]).expect("fits");
    }

    #[test]
    fn refuses_ranges_past_the_end() {
        let mut memory = Memory::new(0x50, Variant::C02, 0);
        let eeprom = At24c::new(0x50, Variant::C02).expect("address");
        let mut buffer = [0u8; 2];
        assert_eq!(
            eeprom.read(&mut memory, 0xff, &mut buffer),
            Err(Error::OutOfRange)
        );
        assert_eq!(
            eeprom.write_page(&mut memory, 0x100, &[1]),
            Err(Error::OutOfRange)
        );
        eeprom
            .read(&mut memory, 0xfe, &mut buffer)
            .expect("last two");
        assert_eq!(buffer, [0xfe, 0xff]);
    }

    #[test]
    fn one_byte_parts_address_each_block_as_a_device() {
        let mut memory = Memory::new(0x50, Variant::C16, 0);
        let eeprom = At24c::new(0x50, Variant::C16).expect("address");
        eeprom
            .write_page(&mut memory, 0x3f0, &[9, 8])
            .expect("write");
        assert_eq!(memory.writes, [(0x53, std::vec![0xf0, 9, 8])]);
        memory.writes.clear();
        let mut buffer = [0u8; 4];
        eeprom.read(&mut memory, 0x1fe, &mut buffer).expect("read");
        assert_eq!(buffer, [0xfe, 0xff, 0x00, 0x01]);
        assert_eq!(
            memory.writes,
            [(0x51, std::vec![0xfe]), (0x52, std::vec![0x00])]
        );
    }

    #[test]
    fn two_byte_parts_send_the_word_address_high_first() {
        let mut memory = Memory::new(0x50, Variant::C32, 0);
        let eeprom = At24c::new(0x50, Variant::C32).expect("address");
        eeprom.write_page(&mut memory, 0x0123, &[7]).expect("write");
        assert_eq!(memory.writes, [(0x50, std::vec![0x01, 0x23, 7])]);
        let mut buffer = [0u8; 2];
        eeprom.read(&mut memory, 0x0123, &mut buffer).expect("read");
        assert_eq!(buffer, [7, 0x24]);
    }

    #[test]
    fn is_not_ready_while_programming() {
        let mut memory = Memory::new(0x50, Variant::C02, 2);
        let eeprom = At24c::new(0x50, Variant::C02).expect("address");
        assert!(eeprom.is_ready(&mut memory).expect("idle"));
        eeprom.write_page(&mut memory, 0, &[1]).expect("write");
        assert!(!eeprom.is_ready(&mut memory).expect("busy"));
        assert!(!eeprom.is_ready(&mut memory).expect("busy"));
        assert!(eeprom.is_ready(&mut memory).expect("done"));
    }
}
