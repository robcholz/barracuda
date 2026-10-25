//! The driver against an emulated ESP SDIO slave running ESP-Hosted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

extern crate std;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;

use embassy_futures::block_on;
use embassy_futures::select::{Either, select};
use embassy_time::Duration;
use sdio::{
    BlockReadCommand, BlockWriteCommand, BusWidth, ByteReadCommand, ByteWriteCommand,
    ControlCommand, MmcBus, MmcError, Response,
};

use crate::frame::{self, Header, Interface, Layout, Parsed};
use crate::init::CHIP_ESP32C6;
use crate::protobuf::{Reader, Writer};
use crate::transport::{self, FIFO_END, SdioTransport, register};
use crate::{Config, State, serial};

/// The INIT event an ESP-Hosted-MCU 2.x C6 sends (see `init.rs`).
const INIT_2X: [u8; 23] = [
    0x22, 21, 0x11, 1, 0x01, 0x12, 1, 0x0d, 0x13, 1, 0x00, 0x14, 1, 20, 0x15, 1, 20, 0x16, 4, 0x10,
    0x00, 0x00, 0x00,
];

const STATION_MAC: [u8; 6] = [0x60, 0x55, 0xf9, 0x10, 0x20, 0x30];
const ACCESS_POINT_MAC: [u8; 6] = [0x60, 0x55, 0xf9, 0x10, 0x20, 0x31];

/// An ESP SDIO slave: CCCR and FBR bytes, the SLC host registers, and the
/// packet FIFO, with ESP-Hosted's behavior behind it.
struct Slave {
    cccr: [u8; 0x200],
    interrupts: u32,
    cleared: Vec<u32>,
    packet_len: u32,
    token: u32,
    to_host: VecDeque<u8>,
    writing: Vec<u8>,
    write_end: u32,
    from_host: Vec<Vec<u8>>,
    opened: bool,
    respond: bool,
    requests: Vec<u32>,
}

impl Default for Slave {
    fn default() -> Self {
        Self {
            cccr: [0; 0x200],
            interrupts: 0,
            cleared: Vec::new(),
            packet_len: 0,
            token: 0,
            to_host: VecDeque::new(),
            writing: Vec::new(),
            write_end: 0,
            from_host: Vec::new(),
            opened: false,
            respond: false,
            requests: Vec::new(),
        }
    }
}

impl Slave {
    fn queue(&mut self, interface: Interface, packet_type: u8, payload: &[u8]) {
        let mut header = Header::new(interface, 0, 0);
        header.packet_type = packet_type;
        let mut out = vec![0; frame::HEADER_LEN + payload.len()];
        frame::encode_packet(header, payload, &mut out).unwrap();
        self.packet_len = self.packet_len.wrapping_add(out.len() as u32);
        self.to_host.extend(out);
        self.interrupts |= transport::INTERRUPT_NEW_PACKET;
    }

    fn register_read(&self, address: u32, out: &mut [u8]) {
        for (index, byte) in out.iter_mut().enumerate() {
            let at = address + index as u32;
            let (word, shift) = match at & !3 {
                register::INT_RAW => (self.interrupts, at & 3),
                register::PACKET_LEN => (self.packet_len, at & 3),
                register::TOKEN_RDATA => (self.token << 16, at & 3),
                _ => (0, 0),
            };
            *byte = (word >> (shift * 8)) as u8;
        }
    }

    fn function1_write(&mut self, address: u32, data: &[u8]) {
        if address < 0x400 {
            if address == register::INT_CLEAR {
                let value = u32::from_le_bytes(data[..4].try_into().unwrap());
                self.cleared.push(value);
                self.interrupts &= !value;
            }
            if address == register::HOST_TO_SLAVE_INTERRUPT && data == [0x01] {
                self.opened = true;
                self.queue(Interface::Private, 0x33, &INIT_2X);
            }
            return;
        }
        if self.writing.is_empty() {
            self.write_end = address;
        }
        assert_eq!(address, self.write_end, "FIFO writes must be contiguous");
        self.writing.extend_from_slice(data);
        self.write_end = address + data.len() as u32;
        if self.write_end == FIFO_END {
            let packet = core::mem::take(&mut self.writing);
            self.receive(&packet);
            self.from_host.push(packet);
        }
    }

    fn function1_read(&mut self, address: u32, out: &mut [u8]) {
        if address < 0x400 {
            self.register_read(address, out);
            return;
        }
        assert!(address + out.len() as u32 <= FIFO_END);
        for byte in out {
            *byte = self.to_host.pop_front().unwrap_or(0);
        }
    }

    /// ESP-Hosted's side of a host packet: answer RPC requests.
    fn receive(&mut self, packet: &[u8]) {
        // Raw FIFO writes in the transport tests carry no packet.
        let Ok(Parsed::Packet(packet, _)) = frame::parse_packet(packet, Layout::Contiguous, true)
        else {
            return;
        };
        if packet.header.interface != Interface::Serial || !self.respond {
            return;
        }
        let (_, data) = serial::parse(packet.payload).unwrap();
        let mut id = 0;
        let mut uid = 0;
        for field in Reader::new(data) {
            match field.unwrap() {
                (2, value) => id = value.as_u32(),
                (3, value) => uid = value.as_u32(),
                _ => {}
            }
        }
        self.requests.push(id);
        let response_id = id + 0x100;
        let mut rpc = Writer::new();
        rpc.uint(1, 2)
            .uint(2, u64::from(response_id))
            .uint(3, u64::from(uid));
        let mac_interface = Reader::new(data)
            .filter_map(Result::ok)
            .find(|(field, _)| *field == 0x101)
            .map(|(_, value)| Reader::new(value.as_bytes()).count());
        rpc.message(response_id, |payload| {
            if id == 0x101 {
                let mac = if mac_interface == Some(0) {
                    STATION_MAC
                } else {
                    ACCESS_POINT_MAC
                };
                payload.bytes(1, &mac);
            }
        });
        let message = serial::compose(&rpc.finish());
        self.queue(Interface::Serial, 0, &message);
        if id == 0x11a {
            // esp_wifi_connect succeeds: ESP-IDF reports the association,
            // then the access point's frames flow to the station.
            let mut event = Writer::new();
            event.uint(1, 3).uint(2, 0x307);
            event.message(0x307, |payload| {
                payload.message(2, |connected| {
                    connected.bytes(1, b"lab").uint(2, 3).uint(4, 6).uint(5, 3);
                });
            });
            let mut message = serial::compose(&event.finish());
            message[3..9].copy_from_slice(b"RPCEvt");
            self.queue(Interface::Serial, 0, &message);
            self.queue(Interface::Station, 0, &[0xaa; 60]);
        }
    }
}

fn arg_fields(arg: u32) -> (u8, u32) {
    (((arg >> 28) & 0x7) as u8, (arg >> 9) & 0x1ffff)
}

fn words(value: u32) -> [u32; 4] {
    [value, 0, 0, 0]
}

impl MmcBus for Slave {
    async fn send_command<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ControlCommand + 'a,
    {
        let arg = cmd.arg();
        let response = match C::INDEX {
            0 | 7 => 0,
            // IO_SEND_OP_COND: ready, one function, 2.7-3.6 V.
            5 => 0x9000_0000 | 0x00ff_8000,
            // SEND_RELATIVE_ADDR: RCA 1.
            3 => 0x0001_0000,
            52 => {
                let (function, address) = arg_fields(arg);
                let write = arg & (1 << 31) != 0;
                let data = (arg & 0xff) as u8;
                if function == 0 {
                    let index = address as usize;
                    if write {
                        self.cccr[index] = data;
                        // Function 1 is ready as soon as it is enabled.
                        if index == 0x02 {
                            self.cccr[0x03] = data;
                        }
                    }
                    u32::from(self.cccr[index])
                } else if write {
                    self.function1_write(address, &[data]);
                    u32::from(data)
                } else {
                    let mut byte = [0];
                    self.register_read(address, &mut byte);
                    u32::from(byte[0])
                }
            }
            other => panic!("unexpected CMD{other}"),
        };
        Ok(<C::Resp<'a> as Response>::from_words(&words(response)))
    }

    async fn read_blocks<'a, C>(
        &mut self,
        mut cmd: C,
        _auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockReadCommand + 'a,
    {
        let (_, address) = arg_fields(cmd.arg());
        self.function1_read(address, &mut cmd.buf()[..]);
        Ok(<C::Resp<'a> as Response>::from_words(&words(0)))
    }

    async fn write_blocks<'a, C>(
        &mut self,
        cmd: C,
        _auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockWriteCommand + 'a,
    {
        let (_, address) = arg_fields(cmd.arg());
        let data = cmd.buf()[..].to_vec();
        self.function1_write(address, &data);
        Ok(<C::Resp<'a> as Response>::from_words(&words(0)))
    }

    async fn read_bytes<'a, C>(&mut self, mut cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteReadCommand + 'a,
    {
        let (_, address) = arg_fields(cmd.arg());
        self.function1_read(address, &mut cmd.buf()[..]);
        Ok(<C::Resp<'a> as Response>::from_words(&words(0)))
    }

    async fn write_bytes<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteWriteCommand + 'a,
    {
        let (_, address) = arg_fields(cmd.arg());
        let data = cmd.buf()[..].to_vec();
        self.function1_write(address, &data);
        Ok(<C::Resp<'a> as Response>::from_words(&words(0)))
    }

    async fn init_idle(&mut self, _hz: u32) -> Result<(), MmcError> {
        Ok(())
    }

    async fn wait_for_event(&mut self) -> Result<(), MmcError> {
        embassy_futures::yield_now().await;
        Ok(())
    }

    fn set_bus(&mut self, _width: BusWidth, _hz: u32) -> Result<(), MmcError> {
        Ok(())
    }

    fn supports_mmc(&self) -> bool {
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

#[derive(Default)]
struct Pin {
    levels: Vec<bool>,
}

impl embedded_hal::digital::ErrorType for Pin {
    type Error = Infallible;
}

impl embedded_hal::digital::OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        self.levels.push(false);
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        self.levels.push(true);
        Ok(())
    }
}

fn transport(slave: &mut Slave) -> SdioTransport<&mut Slave, NoDelay> {
    block_on(SdioTransport::new(
        slave,
        NoDelay,
        40_000_000,
        1,
        Duration::from_millis(1),
    ))
    .unwrap()
}

#[test]
fn function_one_is_enabled_with_its_interrupt_and_512_byte_blocks() {
    let mut slave = Slave::default();
    let mut link = transport(&mut slave);
    block_on(link.open_data_path()).unwrap();
    assert_eq!(slave.cccr[0x02] & 0x02, 0x02, "function 1 enabled");
    assert_eq!(slave.cccr[0x04], 0x03, "master and function 1 interrupts");
    assert_eq!(&slave.cccr[0x10..0x12], &[0x00, 0x02], "function 0 blocks");
    assert_eq!(
        &slave.cccr[0x110..0x112],
        &[0x00, 0x02],
        "function 1 blocks"
    );
    assert!(
        slave.opened,
        "ESP_OPEN_DATA_PATH written to scratch register 7"
    );
}

#[test]
fn status_reads_clear_interrupts_and_count_only_new_bytes() {
    // Bytes the slave sent before: the counter is cumulative and wraps at
    // 1 MiB.
    let mut slave = Slave {
        packet_len: 0x000f_fff0,
        ..Slave::default()
    };
    let mut link = transport(&mut slave);
    block_on(link.open_data_path()).unwrap();
    let status = block_on(link.status()).unwrap();
    assert_eq!(status.interrupts, transport::INTERRUPT_NEW_PACKET);
    // The INIT packet: header and 23 payload bytes, counted across the wrap
    // from a counter the host never read (it starts at zero).
    assert_eq!(status.pending, (0x000f_fff0 + 35) % 0x0010_0000);

    let mut slave = Slave::default();
    let mut link = transport(&mut slave);
    block_on(link.open_data_path()).unwrap();
    let status = block_on(link.status()).unwrap();
    assert_eq!(status.pending, 35);
    let mut buffer = aligned::Aligned::<aligned::A4, _>([0_u8; 36]);
    block_on(link.read(transport::aligned_mut(&mut buffer, 35))).unwrap();
    assert_eq!(block_on(link.status()).unwrap().pending, 0);
    assert_eq!(slave.cleared, [transport::INTERRUPT_NEW_PACKET]);
    let Parsed::Packet(packet, _) =
        frame::parse_packet(&buffer[..35], Layout::Contiguous, true).unwrap()
    else {
        panic!("INIT packet")
    };
    assert_eq!(packet.header.interface, Interface::Private);
    assert_eq!(packet.payload, INIT_2X);
}

#[test]
fn credits_follow_the_token_register_and_writes_end_at_the_fifo_end() {
    assert_eq!(transport::free_buffers(5 << 16, 3), 2);
    assert_eq!(transport::free_buffers(2 << 16, 0xffe), 4);
    assert_eq!(transport::pending_bytes(10, 0x000f_fffe), Ok(12));
    assert!(transport::pending_bytes(u32::MAX, 0).is_err());

    let mut slave = Slave {
        token: 2,
        ..Slave::default()
    };
    let mut link = transport(&mut slave);
    assert!(block_on(link.take_credits(1)).unwrap());
    let mut packet = aligned::Aligned::<aligned::A4, _>([7_u8; 600]);
    block_on(link.write(transport::aligned(&packet, 600), 1)).unwrap();
    assert!(block_on(link.take_credits(1)).unwrap());
    packet[0] = 1;
    block_on(link.write(transport::aligned(&packet, 20), 1)).unwrap();
    // Both buffers are used and the token register still says 2.
    assert!(!block_on(link.take_credits(1)).unwrap());
    // 600 bytes: one 512-byte block at 0x1F800 - 600, then 88 bytes; the
    // mock checks every write ends at the FIFO end.
    assert_eq!(slave.from_host.len(), 2);
    assert_eq!(slave.from_host[0].len(), 600);
    assert_eq!(slave.from_host[1].len(), 20);
}

#[test]
fn the_runner_completes_the_handshake_and_starts_wifi() {
    let mut slave = Slave {
        token: 20,
        respond: true,
        ..Slave::default()
    };
    let mut state = Box::new(State::new());
    let mut pin = Pin::default();
    let (_devices, control, runner) = crate::new(
        &mut state,
        &mut slave,
        NoDelay,
        &mut pin,
        Config {
            frequency_hz: 40_000_000,
            expected_chip: CHIP_ESP32C6,
        },
    );
    let outcome = block_on(select(runner.run(), async {
        let started = control.wait_started().await.unwrap();
        let macs = control.start_wifi().await.unwrap();
        (started, macs)
    }));
    let Either::Second((started, macs)) = outcome;
    assert_eq!(started.chip, CHIP_ESP32C6);
    assert_eq!(started.firmware, None);
    assert_eq!(macs.station, STATION_MAC);
    assert_eq!(macs.access_point, ACCESS_POINT_MAC);
    assert_eq!(pin.levels, [false, true], "EN held low, then released");

    // The host answered the INIT event first, then ran ESP-IDF's start-up.
    let reply = &slave.from_host[0];
    let Ok(Parsed::Packet(packet, _)) = frame::parse_packet(reply, Layout::Contiguous, true) else {
        panic!("host INIT reply")
    };
    assert_eq!(packet.header.interface, Interface::Private);
    assert_eq!(
        packet.payload,
        [
            0x22, 15, 0x44, 1, 0, 0x45, 1, 0x0d, 0x46, 1, 0, 0x47, 1, 80, 0x48, 1, 60
        ]
    );
    assert_eq!(slave.requests, [0x116, 0x104, 0x118, 0x101, 0x101]);
}

#[test]
fn a_co_processor_of_another_chip_is_refused_at_start() {
    let mut slave = Slave {
        token: 20,
        ..Slave::default()
    };
    let mut state = Box::new(State::new());
    let mut pin = Pin::default();
    let (_devices, control, runner) = crate::new(
        &mut state,
        &mut slave,
        NoDelay,
        &mut pin,
        Config {
            frequency_hz: 40_000_000,
            expected_chip: 0x17,
        },
    );
    let outcome = block_on(select(runner.run(), control.wait_started()));
    let Either::Second(result) = outcome;
    assert_eq!(
        result,
        Err(crate::StartError::Incompatible(
            crate::init::Incompatible::Chip {
                expected: 0x17,
                reported: 0x0d
            }
        ))
    );
    assert!(matches!(
        block_on(control.scan(5)),
        Err(crate::ControlError::NotStarted(_))
    ));
}

#[test]
fn a_join_raises_the_station_link_and_carries_its_frames() {
    use embassy_net_driver_channel::driver::{Driver, LinkState, RxToken};

    let mut slave = Slave {
        token: 20,
        respond: true,
        ..Slave::default()
    };
    let mut state = Box::new(State::new());
    let mut pin = Pin::default();
    let (mut devices, control, runner) = crate::new(
        &mut state,
        &mut slave,
        NoDelay,
        &mut pin,
        Config {
            frequency_hz: 40_000_000,
            expected_chip: CHIP_ESP32C6,
        },
    );
    let Either::Second(joined) = block_on(select(runner.run(), async {
        control.wait_started().await.unwrap();
        control.start_wifi().await.unwrap();
        let joined = control
            .connect("lab", "secret12", Duration::from_secs(5))
            .await;
        // Let the runner hand the frame that followed the event to the stack.
        embassy_time::Timer::after_millis(200).await;
        joined
    }));
    assert_eq!(joined, Ok(()));
    assert!(control.station_connected());

    let mut context = core::task::Context::from_waker(core::task::Waker::noop());
    assert!(devices.station.link_state(&mut context) == LinkState::Up);
    let (frame, _) = devices
        .station
        .receive(&mut context)
        .expect("station frame");
    assert_eq!(frame.consume(|bytes| bytes.to_vec()), [0xaa; 60]);
    assert_eq!(
        slave.requests,
        [0x116, 0x104, 0x118, 0x101, 0x101, 0x11c, 0x11a]
    );
}
