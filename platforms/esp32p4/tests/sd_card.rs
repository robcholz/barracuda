//! The SD card protocol against a modelled card on a modelled SD host slot.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::{cell::RefCell, rc::Rc};

use barracuda_platform_esp32p4::sd_card::{
    transfer_blocks, SdCard, BLOCK_BYTES, DEFAULT_SPEED_HZ, IDENTIFICATION_FREQUENCY_HZ,
    TRANSFER_BLOCKS,
};
use embassy_futures::block_on;
use embedded_io::{Error as _, ErrorKind, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use sdio::{
    BlockReadCommand, BlockWriteCommand, BusWidth, ByteReadCommand, ByteWriteCommand,
    ControlCommand, MmcBus, MmcError, Response,
};

const RCA: u32 = 0x1234;
/// Card status: ready for data in the transfer state.
const READY: u32 = (1 << 8) | (4 << 9);
const HIGH_SPEED_SWITCH: u32 = 0x80FF_FFF1;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Idle(u32),
    Bus(u8, u32),
    Command { app: bool, index: u8, arg: u32 },
    Read { index: u8, arg: u32, blocks: u32 },
    Write { index: u8, arg: u32, blocks: u32 },
}

/// One card in the slot, as the SD Physical Layer describes it.
struct Model {
    memory: Vec<u8>,
    high_capacity: bool,
    four_bit: bool,
    high_speed: bool,
    present: bool,
    power_up_polls: usize,
    app: bool,
    events: Vec<Event>,
}

impl Model {
    fn sdhc() -> Self {
        Self {
            // C_SIZE 0: (0 + 1) * 1024 blocks.
            memory: (0..1024 * BLOCK_BYTES)
                .map(|byte| (byte % 251) as u8)
                .collect(),
            high_capacity: true,
            four_bit: true,
            high_speed: true,
            present: true,
            power_up_polls: 2,
            app: false,
            events: Vec::new(),
        }
    }

    fn sdsc() -> Self {
        Self {
            // C_SIZE 15, C_SIZE_MULT 0: 16 * 4 blocks.
            memory: (0..64 * BLOCK_BYTES)
                .map(|byte| (byte % 241) as u8)
                .collect(),
            high_capacity: false,
            ..Self::sdhc()
        }
    }

    fn csd(&self) -> [u32; 4] {
        let csd: u128 = if self.high_capacity {
            (1 << 126) | (9 << 80)
        } else {
            (9 << 80) | (15 << 62)
        };
        [
            csd as u32,
            (csd >> 32) as u32,
            (csd >> 64) as u32,
            (csd >> 96) as u32,
        ]
    }

    fn byte_offset(&self, arg: u32) -> usize {
        if self.high_capacity {
            arg as usize * BLOCK_BYTES
        } else {
            arg as usize
        }
    }

    fn command(&mut self, index: u8, arg: u32) -> Result<[u32; 4], MmcError> {
        let app = core::mem::take(&mut self.app);
        self.events.push(Event::Command { app, index, arg });
        if !self.present {
            return Err(MmcError::Timeout);
        }
        let word = match (app, index) {
            (false, 0) => 0,
            (false, 8) => arg & 0xFFF,
            (_, 55) => {
                self.app = true;
                READY | (1 << 5)
            }
            (true, 41) => {
                if self.power_up_polls > 0 {
                    self.power_up_polls -= 1;
                    0x00FF_8000
                } else {
                    0x80FF_8000 | (u32::from(self.high_capacity) << 30)
                }
            }
            (false, 2) => 0,
            (false, 3) => RCA << 16,
            (false, 9) => return Ok(self.csd()),
            (true, 6) | (false, 7 | 12 | 13 | 16) => READY,
            _ => return Err(MmcError::Timeout),
        };
        Ok([word, 0, 0, 0])
    }

    fn read(&mut self, index: u8, arg: u32, buffer: &mut [u8]) -> Result<u32, MmcError> {
        let app = core::mem::take(&mut self.app);
        let blocks = (buffer.len() / BLOCK_BYTES) as u32;
        self.events.push(Event::Read { index, arg, blocks });
        if !self.present {
            return Err(MmcError::Timeout);
        }
        match (app, index) {
            (true, 51) => {
                buffer.fill(0);
                buffer[0] = 0x02;
                buffer[1] = if self.four_bit { 0x05 } else { 0x01 };
            }
            (false, 6) => {
                assert_eq!(arg, HIGH_SPEED_SWITCH);
                buffer.fill(0);
                buffer[16] = if self.high_speed { 0x01 } else { 0x0F };
            }
            (false, 17 | 18) => {
                let offset = self.byte_offset(arg);
                buffer.copy_from_slice(&self.memory[offset..offset + buffer.len()]);
            }
            _ => return Err(MmcError::Timeout),
        }
        Ok(READY)
    }

    fn write(&mut self, index: u8, arg: u32, buffer: &[u8]) -> Result<u32, MmcError> {
        self.app = false;
        let blocks = (buffer.len() / BLOCK_BYTES) as u32;
        self.events.push(Event::Write { index, arg, blocks });
        if !self.present || !matches!(index, 24 | 25) {
            return Err(MmcError::Timeout);
        }
        let offset = self.byte_offset(arg);
        self.memory[offset..offset + buffer.len()].copy_from_slice(buffer);
        Ok(READY)
    }
}

/// A four-line slot whose host sends CMD12 by itself, as esp-hal's does.
#[derive(Clone)]
struct Slot(Rc<RefCell<Model>>);

impl MmcBus for Slot {
    async fn send_command<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ControlCommand + 'a,
    {
        let words = self.0.borrow_mut().command(C::INDEX, cmd.arg())?;
        Ok(<C::Resp<'a> as Response>::from_words(&words))
    }

    async fn read_blocks<'a, C>(
        &mut self,
        mut cmd: C,
        _auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockReadCommand + 'a,
    {
        let arg = cmd.arg();
        let status = self.0.borrow_mut().read(C::INDEX, arg, cmd.buf())?;
        Ok(<C::Resp<'a> as Response>::from_words(&[status, 0, 0, 0]))
    }

    async fn write_blocks<'a, C>(
        &mut self,
        cmd: C,
        _auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockWriteCommand + 'a,
    {
        let status = self.0.borrow_mut().write(C::INDEX, cmd.arg(), cmd.buf())?;
        Ok(<C::Resp<'a> as Response>::from_words(&[status, 0, 0, 0]))
    }

    async fn read_bytes<'a, C>(&mut self, _cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteReadCommand + 'a,
    {
        Err(MmcError::Unsupported)
    }

    async fn write_bytes<'a, C>(&mut self, _cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteWriteCommand + 'a,
    {
        Err(MmcError::Unsupported)
    }

    async fn init_idle(&mut self, hz: u32) -> Result<(), MmcError> {
        self.0.borrow_mut().events.push(Event::Idle(hz));
        Ok(())
    }

    fn set_bus(&mut self, width: BusWidth, hz: u32) -> Result<(), MmcError> {
        let lines = match width {
            BusWidth::W1 => 1,
            BusWidth::W4 => 4,
            BusWidth::W8 => 8,
        };
        self.0.borrow_mut().events.push(Event::Bus(lines, hz));
        Ok(())
    }

    fn supports_mmc(&self) -> bool {
        true
    }

    fn supports_auto_stop(&self) -> bool {
        true
    }

    fn supports_bus_width(&self) -> BusWidth {
        BusWidth::W4
    }

    fn supports_frequency(&self) -> u32 {
        40_000_000
    }
}

struct NoDelay;

impl embedded_hal_async::delay::DelayNs for NoDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

fn inserted(model: Model, four_bit: bool) -> (SdCard<Slot, NoDelay>, Rc<RefCell<Model>>) {
    let model = Rc::new(RefCell::new(model));
    let card = SdCard::new(Slot(Rc::clone(&model)), NoDelay, four_bit, 40_000_000);
    (card, model)
}

fn events(model: &Rc<RefCell<Model>>) -> Vec<Event> {
    core::mem::take(&mut model.borrow_mut().events)
}

#[test]
fn a_transfer_never_spans_more_than_its_bound() {
    assert_eq!(transfer_blocks(0, 1), 1);
    assert_eq!(transfer_blocks(511, 2), 2);
    assert_eq!(transfer_blocks(0, BLOCK_BYTES), 1);
    assert_eq!(transfer_blocks(0, BLOCK_BYTES * 3), 3);
    assert_eq!(transfer_blocks(100, 1 << 20), TRANSFER_BLOCKS);
}

#[test]
fn a_card_is_identified_on_four_lines_at_high_speed() {
    let (mut card, model) = inserted(Model::sdhc(), true);
    assert_eq!(card.capacity(), None);
    let mut byte = [0_u8; 1];
    assert_eq!(block_on(card.read(&mut byte)).expect("first read"), 1);
    assert_eq!(card.capacity(), Some(1024 * BLOCK_BYTES as u64));

    let log = events(&model);
    assert_eq!(log[0], Event::Idle(IDENTIFICATION_FREQUENCY_HZ));
    // ACMD41 polled until the card is powered up.
    let power_up = log
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Command {
                    app: true,
                    index: 41,
                    ..
                }
            )
        })
        .count();
    assert_eq!(power_up, 3);
    // Four lines at the default speed, then high speed after CMD6.
    let bus: Vec<_> = log
        .iter()
        .filter(|event| matches!(event, Event::Bus(..)))
        .cloned()
        .collect();
    assert_eq!(
        bus,
        [Event::Bus(4, DEFAULT_SPEED_HZ), Event::Bus(4, 40_000_000)]
    );
    assert!(log.contains(&Event::Command {
        app: true,
        index: 6,
        arg: 0b10
    }));
    assert!(log.contains(&Event::Command {
        app: false,
        index: 16,
        arg: 512
    }));
    assert_eq!(
        log.last(),
        Some(&Event::Read {
            index: 17,
            arg: 0,
            blocks: 1
        })
    );
}

#[test]
fn a_card_without_high_speed_or_a_one_line_board_stays_slower() {
    let mut model = Model::sdhc();
    model.high_speed = false;
    let (mut card, model) = inserted(model, true);
    block_on(card.seek(SeekFrom::Start(0))).expect("identified");
    let bus: Vec<_> = events(&model)
        .into_iter()
        .filter(|event| matches!(event, Event::Bus(..)))
        .collect();
    assert_eq!(bus, [Event::Bus(4, DEFAULT_SPEED_HZ)]);

    let (mut card, model) = inserted(Model::sdhc(), false);
    block_on(card.seek(SeekFrom::Start(0))).expect("identified");
    let log = events(&model);
    // No SCR read, no ACMD6: the Board wires one line.
    assert!(!log.iter().any(|event| matches!(
        event,
        Event::Read { index: 51, .. }
            | Event::Command {
                app: true,
                index: 6,
                ..
            }
    )));
    assert!(log.contains(&Event::Bus(1, 40_000_000)));
}

#[test]
fn reads_move_at_most_eight_blocks_per_transfer() {
    let (mut card, model) = inserted(Model::sdhc(), true);
    block_on(card.seek(SeekFrom::Start(100))).expect("seek");
    events(&model);

    let mut buffer = vec![0_u8; 10 * 1024];
    let read = block_on(card.read(&mut buffer)).expect("read");
    assert_eq!(read, buffer.len());
    let expected = model.borrow().memory[100..100 + buffer.len()].to_vec();
    assert_eq!(buffer, expected);

    let transfers: Vec<_> = events(&model);
    assert!(transfers.iter().all(
        |event| matches!(event, Event::Read { blocks, .. } if *blocks as usize <= TRANSFER_BLOCKS)
    ));
    assert_eq!(
        transfers,
        [
            Event::Read {
                index: 18,
                arg: 0,
                blocks: 8
            },
            Event::Read {
                index: 18,
                arg: 8,
                blocks: 8
            },
            Event::Read {
                index: 18,
                arg: 16,
                blocks: 5
            },
        ]
    );
    assert_eq!(
        block_on(card.seek(SeekFrom::Current(0))).expect("position"),
        100 + buffer.len() as u64
    );
}

#[test]
fn writes_keep_the_bytes_around_them_and_wait_for_programming() {
    let (mut card, model) = inserted(Model::sdhc(), true);
    block_on(card.seek(SeekFrom::Start(700))).expect("seek");
    let before = model.borrow().memory.clone();
    events(&model);

    let data: Vec<u8> = (0..5000).map(|byte| (byte % 7) as u8 + 1).collect();
    assert_eq!(block_on(card.write(&data)).expect("write"), data.len());

    let after = model.borrow().memory.clone();
    assert_eq!(&after[700..5700], &data[..]);
    assert_eq!(&after[..700], &before[..700]);
    assert_eq!(&after[5700..], &before[5700..]);

    let log = events(&model);
    // 700..5700 spans blocks 1 through 11: one transfer of eight, one of three.
    let writes: Vec<_> = log
        .iter()
        .filter(|event| matches!(event, Event::Write { .. }))
        .cloned()
        .collect();
    assert_eq!(
        writes,
        [
            Event::Write {
                index: 25,
                arg: 1,
                blocks: 8
            },
            Event::Write {
                index: 25,
                arg: 9,
                blocks: 3
            },
        ]
    );
    // The partial first and last blocks are read first, and each write is
    // followed by a status poll until the card is ready.
    assert!(log.contains(&Event::Read {
        index: 17,
        arg: 1,
        blocks: 1
    }));
    assert!(log.contains(&Event::Read {
        index: 17,
        arg: 11,
        blocks: 1
    }));
    let polls = log
        .iter()
        .filter(|event| matches!(event, Event::Command { index: 13, .. }))
        .count();
    assert_eq!(polls, 2);
}

#[test]
fn a_standard_capacity_card_is_addressed_in_bytes() {
    let (mut card, model) = inserted(Model::sdsc(), true);
    assert_eq!(
        block_on(card.seek(SeekFrom::End(0))).expect("end"),
        64 * BLOCK_BYTES as u64
    );
    block_on(card.seek(SeekFrom::Start(3 * BLOCK_BYTES as u64))).expect("seek");
    events(&model);
    let mut block = [0_u8; BLOCK_BYTES];
    block_on(card.read(&mut block)).expect("read");
    assert_eq!(
        events(&model),
        [Event::Read {
            index: 17,
            arg: 3 * BLOCK_BYTES as u32,
            blocks: 1
        }]
    );
    // Nothing is read past the end of the card.
    block_on(card.seek(SeekFrom::End(-10))).expect("near the end");
    assert_eq!(block_on(card.read(&mut block)).expect("short read"), 10);
}

#[test]
fn a_removed_card_fails_and_a_new_one_is_identified() {
    let (mut card, model) = inserted(Model::sdhc(), true);
    let mut byte = [0_u8; 1];
    block_on(card.read(&mut byte)).expect("first card");

    model.borrow_mut().present = false;
    let error = block_on(card.read(&mut byte)).expect_err("removed card");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert_eq!(card.capacity(), None);
    assert!(block_on(card.seek(SeekFrom::Start(0))).is_err());

    {
        let mut model = model.borrow_mut();
        model.present = true;
        model.power_up_polls = 1;
    }
    events(&model);
    block_on(card.read(&mut byte)).expect("new card");
    assert_eq!(events(&model)[0], Event::Idle(IDENTIFICATION_FREQUENCY_HZ));

    // A bad position is the caller's mistake: the card stays identified.
    let error = block_on(card.seek(SeekFrom::End(1))).expect_err("past the end");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(card.capacity().is_some());
}
