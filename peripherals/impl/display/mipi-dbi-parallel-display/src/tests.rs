//! Interface behavior of the parallel-bus adapter against a recording bus.

extern crate std;

use std::vec::Vec;

use barracuda_peripheral::display::ParallelOutput;
use mipidsi::interface::Interface;

use super::{ParallelInterface, STAGING_BYTES};

/// One recorded bus write.
#[derive(Debug, PartialEq, Eq)]
enum Write {
    Command(u8, Vec<u8>),
    Data(Vec<u8>),
}

#[derive(Default)]
struct Bus {
    writes: Vec<Write>,
}

impl ParallelOutput for &mut Bus {
    type Error = core::convert::Infallible;

    fn capacity(&self) -> usize {
        4096
    }

    fn write_command(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error> {
        self.writes.push(Write::Command(command, data.to_vec()));
        Ok(())
    }

    fn write_data(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.writes.push(Write::Data(data.to_vec()));
        Ok(())
    }
}

fn data_bytes(writes: &[Write]) -> Vec<u8> {
    writes
        .iter()
        .flat_map(|write| match write {
            Write::Data(bytes) => bytes.clone(),
            Write::Command(..) => Vec::new(),
        })
        .collect()
}

#[test]
fn commands_carry_their_parameters() {
    let mut bus = Bus::default();
    {
        let mut interface = ParallelInterface::new(&mut bus);
        let Ok(()) = interface.send_command(0x3a, &[0x55]);
    }
    assert_eq!(bus.writes, [Write::Command(0x3a, std::vec![0x55])]);
}

#[test]
fn pixels_stream_in_staging_sized_writes_in_order() {
    let mut bus = Bus::default();
    let count = STAGING_BYTES / 2 + 3;
    {
        let mut interface = ParallelInterface::new(&mut bus);
        let pixels = (0..count).map(|index| [(index >> 8) as u8, index as u8]);
        let Ok(()) = interface.send_pixels(pixels);
    }
    assert!(bus.writes.len() >= 2);
    assert!(bus.writes.iter().all(|write| match write {
        Write::Data(bytes) => bytes.len() <= STAGING_BYTES,
        Write::Command(..) => false,
    }));
    let expected: Vec<u8> = (0..count)
        .flat_map(|index| [(index >> 8) as u8, index as u8])
        .collect();
    assert_eq!(data_bytes(&bus.writes), expected);
}

#[test]
fn repeated_pixels_send_exactly_count_copies() {
    let mut bus = Bus::default();
    let count = STAGING_BYTES as u32 + 7;
    {
        let mut interface = ParallelInterface::new(&mut bus);
        let Ok(()) = interface.send_repeated_pixel([0xf8, 0x00], count);
    }
    let bytes = data_bytes(&bus.writes);
    assert_eq!(bytes.len(), count as usize * 2);
    assert!(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pixel| *pixel == [0xf8, 0x00])
    );
}
