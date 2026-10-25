//! The task that owns the SDIO link: start-up, receive, and transmit.

use alloc::vec::Vec;
use core::cell::Cell;

use aligned::{A4, Aligned};
use embassy_futures::select::{Either4, select, select4};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::OutputPin;
use embedded_hal_async::delay::DelayNs;
use sdio::MmcBus;

use crate::control::Control;
use crate::frame::{self, Header, Interface, Layout, Parsed};
use crate::init::{SdioMode, SlaveInit};
use crate::rpc::{self, Event, Message, MessageKind};
use crate::serial::{self, Endpoint, Reassembler};
use crate::transport::{
    CREDIT_BYTES, INTERRUPT_START_THROTTLE, INTERRUPT_STOP_THROTTLE, SdioTransport, TransportError,
    aligned, aligned_mut,
};
use crate::{StartError, Started};

/// Largest Ethernet frame the co-processor carries, without the FCS.
pub const MTU: usize = 1514;
/// Bytes read from the co-processor in one transfer at most.
pub const RX_TRANSFER: usize = 16 * 1024;
/// Largest packet the host sends: a header and an MTU frame, within one
/// 1536-byte co-processor receive buffer.
const TX_TRANSFER: usize = CREDIT_BYTES as usize;

/// How long the co-processor is held in reset (`reset_slave` in
/// `transport_drv.c` holds it 50 ms).
const RESET_HOLD: Duration = Duration::from_millis(50);
/// How long the co-processor needs after reset before its SDIO slave
/// answers; ESP-Hosted's host waits 1.5 s.
const BOOT_TIME: Duration = Duration::from_millis(1500);
/// Card set-up attempts, `RETRY_DELAY` apart.
const CARD_ATTEMPTS: u32 = 5;
const RETRY_DELAY: Duration = Duration::from_millis(200);
/// How long the host waits for the co-processor's INIT event.
const INIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Status is read this often even without a card interrupt, so a missed
/// interrupt cannot stall the link.
const POLL: Duration = Duration::from_millis(50);
/// Transmit-credit polls before a packet is dropped
/// (`MAX_WRITE_BUF_RETRIES` in `sdio_drv.c`).
const CREDIT_POLLS: u32 = 50;

/// Static storage the driver needs; place it in a `static`.
pub struct State {
    station: ch::State<MTU, 4, 4>,
    access_point: ch::State<MTU, 4, 4>,
    shared: Shared,
    rx: Aligned<A4, [u8; RX_TRANSFER]>,
    tx: Aligned<A4, [u8; TX_TRANSFER]>,
}

impl State {
    /// Creates the storage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            station: ch::State::new(),
            access_point: ch::State::new(),
            shared: Shared::new(),
            rx: Aligned([0; RX_TRANSFER]),
            tx: Aligned([0; TX_TRANSFER]),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

type Raw = CriticalSectionRawMutex;

/// State shared between the [`Runner`] and its [`Control`].
pub(crate) struct Shared {
    pub(crate) request: Signal<Raw, Vec<u8>>,
    pub(crate) response: Signal<Raw, Vec<u8>>,
    pub(crate) events: Channel<Raw, Event, 8>,
    pub(crate) rpc: Mutex<Raw, u32>,
    started: BlockingMutex<Raw, Cell<Option<Result<Started, StartError>>>>,
    started_signal: Signal<Raw, ()>,
    station_connected: BlockingMutex<Raw, Cell<bool>>,
}

impl Shared {
    const fn new() -> Self {
        Self {
            request: Signal::new(),
            response: Signal::new(),
            events: Channel::new(),
            rpc: Mutex::new(0),
            started: BlockingMutex::new(Cell::new(None)),
            started_signal: Signal::new(),
            station_connected: BlockingMutex::new(Cell::new(false)),
        }
    }

    pub(crate) async fn started(&self) -> Result<Started, StartError> {
        loop {
            if let Some(result) = self.started.lock(Cell::get) {
                return result;
            }
            self.started_signal.wait().await;
        }
    }

    pub(crate) fn started_now(&self) -> Option<Result<Started, StartError>> {
        self.started.lock(Cell::get)
    }

    fn finish_start(&self, result: Result<Started, StartError>) {
        self.started.lock(|cell| cell.set(Some(result)));
        self.started_signal.signal(());
    }

    pub(crate) fn station_connected(&self) -> bool {
        self.station_connected.lock(Cell::get)
    }

    fn publish(&self, event: Event) {
        if self.events.try_send(event.clone()).is_err() {
            // Keep the newest events: a waiting operation cares about what
            // happens after it started.
            let _ = self.events.try_receive();
            let _ = self.events.try_send(event);
        }
    }
}

/// Where the co-processor is and how fast to clock it.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// SDIO clock once the card is set up (ESP-Hosted's P4 default is 40 MHz).
    pub frequency_hz: u32,
    /// The chip the Board declares (`ESP_PRIV_FIRMWARE_CHIP_*`).
    pub expected_chip: u8,
}

/// One embassy-net device the co-processor carries.
pub type NetDevice<'a> = ch::Device<'a, MTU>;

/// The two network interfaces the co-processor carries.
pub struct Devices<'a> {
    /// The Wi-Fi station's Ethernet frames.
    pub station: NetDevice<'a>,
    /// The soft access point's Ethernet frames.
    pub access_point: NetDevice<'a>,
}

/// Creates the driver: the network devices for embassy-net, the [`Control`]
/// that runs Wi-Fi operations, and the [`Runner`] to spawn as a task.
pub fn new<'a, B, D, R>(
    state: &'a mut State,
    bus: B,
    delay: D,
    reset: R,
    config: Config,
) -> (Devices<'a>, Control<'a>, Runner<'a, B, D, R>)
where
    B: MmcBus,
    D: DelayNs,
    R: OutputPin,
{
    let State {
        station,
        access_point,
        shared,
        rx,
        tx,
    } = state;
    let (station_runner, station_device) = ch::new(station, HardwareAddress::Ethernet([0; 6]));
    let (access_point_runner, access_point_device) =
        ch::new(access_point, HardwareAddress::Ethernet([0; 6]));
    let control = Control::new(
        shared,
        station_runner.state_runner(),
        access_point_runner.state_runner(),
    );
    let runner = Runner {
        bus: Some((bus, delay)),
        reset,
        config,
        station: station_runner,
        access_point: access_point_runner,
        shared,
        rx,
        tx,
    };
    (
        Devices {
            station: station_device,
            access_point: access_point_device,
        },
        control,
        runner,
    )
}

/// Owns the SDIO link; run it as a task for as long as Wi-Fi is used.
pub struct Runner<'a, B: MmcBus, D: DelayNs, R: OutputPin> {
    bus: Option<(B, D)>,
    reset: R,
    config: Config,
    station: ch::Runner<'a, MTU>,
    access_point: ch::Runner<'a, MTU>,
    shared: &'a Shared,
    rx: &'a mut Aligned<A4, [u8; RX_TRANSFER]>,
    tx: &'a mut Aligned<A4, [u8; TX_TRANSFER]>,
}

/// Link state that exists once the co-processor is up.
struct Link<B: MmcBus, D: DelayNs> {
    transport: SdioTransport<B, D>,
    stream: Vec<u8>,
    reassembler: Reassembler,
    sequence: u16,
    layout: Layout,
    verify_checksum: bool,
    /// Credit size of one host transfer in software-aggregation mode.
    aggregate_credit: Option<u32>,
    throttled: bool,
    /// When the co-processor's status was last read.
    last_status: Instant,
}

impl<'a, B: MmcBus, D: DelayNs, R: OutputPin> Runner<'a, B, D, R> {
    /// Resets and starts the co-processor, then moves packets until the
    /// device powers off.
    ///
    /// A co-processor that cannot be started, such as one running firmware
    /// this host cannot drive, is reported through
    /// [`Control::wait_started`]; the runner then idles.
    pub async fn run(mut self) -> ! {
        let mut link = match self.start().await {
            Ok((link, started)) => {
                self.shared.finish_start(Ok(started));
                link
            }
            Err(error) => {
                log::error!("ESP-Hosted co-processor did not start: {error}");
                self.shared.finish_start(Err(error));
                loop {
                    Timer::after(Duration::from_secs(3600)).await;
                }
            }
        };
        loop {
            if let Err(error) = self.serve(&mut link).await {
                log::warn!("ESP-Hosted SDIO transfer failed: {error}");
                Timer::after(RETRY_DELAY).await;
            }
        }
    }

    async fn start(&mut self) -> Result<(Link<B, D>, Started), StartError> {
        // EN is active low: hold the co-processor in reset, release it, and
        // give its firmware time to bring the SDIO slave up.
        self.reset.set_low().map_err(|_| StartError::ResetPin)?;
        Timer::after(RESET_HOLD).await;
        self.reset.set_high().map_err(|_| StartError::ResetPin)?;
        Timer::after(BOOT_TIME).await;

        let (bus, delay) = self.bus.take().ok_or(StartError::ResetPin)?;
        let mut transport = SdioTransport::new(
            bus,
            delay,
            self.config.frequency_hz,
            CARD_ATTEMPTS,
            RETRY_DELAY,
        )
        .await
        .map_err(StartError::Card)?;
        log::info!(
            "ESP-Hosted co-processor answered on SDIO at {} kHz",
            transport.frequency_hz() / 1000
        );
        transport.open_data_path().await.map_err(StartError::Card)?;

        let mut link = Link {
            transport,
            stream: Vec::new(),
            reassembler: Reassembler::new(),
            sequence: 0,
            layout: Layout::Contiguous,
            verify_checksum: false,
            aggregate_credit: None,
            throttled: false,
            last_status: Instant::now(),
        };
        let init = embassy_time::with_timeout(INIT_TIMEOUT, self.wait_init(&mut link))
            .await
            .map_err(|_| StartError::NoInitEvent)??;
        init.check(self.config.expected_chip)
            .map_err(StartError::Incompatible)?;
        match init.sdio_mode().map_err(StartError::Incompatible)? {
            SdioMode::Packets => {}
            SdioMode::Aggregated {
                host_to_device_bytes,
                device_to_host_bytes,
            } => {
                if device_to_host_bytes as usize > RX_TRANSFER {
                    return Err(StartError::AggregateTooLarge(device_to_host_bytes));
                }
                link.layout = Layout::Aligned;
                link.aggregate_credit = Some(host_to_device_bytes);
            }
        }
        link.verify_checksum = init.checksum();
        let mut reply = [0; 32];
        let length = init.host_reply(&mut reply).map_err(StartError::Init)?;
        send(&mut link, self.tx, Interface::Private, &reply[..length])
            .await
            .map_err(StartError::Card)?;
        let firmware = init.firmware_version();
        match firmware {
            Some(version) => log::info!("ESP-Hosted co-processor firmware {version}"),
            None => log::info!(
                "ESP-Hosted co-processor firmware predates 3.x and reports no version in its INIT event"
            ),
        }
        Ok((
            link,
            Started {
                chip: init.chip_id().unwrap_or(0xff),
                firmware,
                checksum: init.checksum(),
                aggregated: link_aggregated(init.sdio_mode()),
            },
        ))
    }

    async fn wait_init(&mut self, link: &mut Link<B, D>) -> Result<SlaveInit, StartError> {
        loop {
            let _ = select(link.transport.wait_interrupt(), Timer::after(POLL)).await;
            let status = link.transport.status().await.map_err(StartError::Card)?;
            if status.pending == 0 {
                continue;
            }
            let length = (status.pending as usize).min(RX_TRANSFER);
            link.transport
                .read(aligned_mut(self.rx, length))
                .await
                .map_err(StartError::Card)?;
            link.stream.extend_from_slice(&self.rx[..length]);
            let mut offset = 0;
            let mut init = None;
            while let Ok(Parsed::Packet(packet, used)) =
                frame::parse_packet(&link.stream[offset..], Layout::Contiguous, false)
            {
                offset += used;
                if packet.header.interface == Interface::Private {
                    init = Some(SlaveInit::parse(packet.payload).map_err(StartError::Init)?);
                }
            }
            link.stream.clear();
            if let Some(init) = init {
                return Ok(init);
            }
        }
    }

    async fn serve(&mut self, link: &mut Link<B, D>) -> Result<(), TransportError> {
        let throttled = link.throttled;
        let Self {
            station,
            access_point,
            shared,
            tx,
            ..
        } = self;
        let ready = select4(
            select(link.transport.wait_interrupt(), Timer::after(POLL)),
            shared.request.wait(),
            async {
                if throttled {
                    core::future::pending::<()>().await;
                }
                station.tx_buf().await
            },
            async {
                if throttled {
                    core::future::pending::<()>().await;
                }
                access_point.tx_buf().await
            },
        )
        .await;
        let interrupted = matches!(ready, Either4::First(_));
        match ready {
            Either4::First(_) => {}
            Either4::Second(request) => {
                send(link, tx, Interface::Serial, &serial::compose(&request)).await?;
            }
            Either4::Third(frame) => {
                let result = send(link, tx, Interface::Station, frame).await;
                station.tx_done();
                result?;
            }
            Either4::Fourth(frame) => {
                let result = send(link, tx, Interface::AccessPoint, frame).await;
                access_point.tx_done();
                result?;
            }
        }
        // Read the co-processor's status on its interrupt, and at least every
        // poll interval while transmitting keeps the loop busy.
        if interrupted || link.last_status.elapsed() >= POLL {
            self.receive(link).await?;
        }
        Ok(())
    }

    async fn receive(&mut self, link: &mut Link<B, D>) -> Result<(), TransportError> {
        loop {
            let status = link.transport.status().await?;
            link.last_status = Instant::now();
            if status.interrupts & INTERRUPT_START_THROTTLE != 0 {
                link.throttled = true;
            }
            if status.interrupts & INTERRUPT_STOP_THROTTLE != 0 {
                link.throttled = false;
            }
            if status.pending == 0 {
                return Ok(());
            }
            let length = (status.pending as usize).min(RX_TRANSFER);
            link.transport.read(aligned_mut(self.rx, length)).await?;
            link.stream.extend_from_slice(&self.rx[..length]);
            self.dispatch(link);
            if link.layout == Layout::Aligned {
                // An aggregate ends with padding after its last packet.
                link.stream.clear();
            }
        }
    }

    fn dispatch(&mut self, link: &mut Link<B, D>) {
        let mut offset = 0;
        loop {
            let rest = link.stream.get(offset..).unwrap_or_default();
            match frame::parse_packet(rest, link.layout, link.verify_checksum) {
                Ok(Parsed::Packet(packet, used)) => {
                    offset += used;
                    let more = packet.header.more_fragments();
                    match packet.header.interface {
                        Interface::Station => deliver(&mut self.station, packet.payload),
                        Interface::AccessPoint => {
                            deliver(&mut self.access_point, packet.payload);
                        }
                        Interface::Serial => match link.reassembler.push(packet.payload, more) {
                            Ok(Some(message)) => self.handle_serial(&message),
                            Ok(None) => {}
                            Err(error) => log::warn!("ESP-Hosted serial message dropped: {error}"),
                        },
                        other => log::debug!("ESP-Hosted packet on {other:?} ignored"),
                    }
                }
                Ok(Parsed::Incomplete) => break,
                Ok(Parsed::End) => {
                    offset = link.stream.len();
                    break;
                }
                Err(error) => {
                    // A stream cannot be resynchronized after a bad header.
                    log::warn!("ESP-Hosted packets dropped: {error}");
                    offset = link.stream.len();
                    break;
                }
            }
        }
        link.stream.drain(..offset.min(link.stream.len()));
    }

    fn handle_serial(&mut self, bytes: &[u8]) {
        let (endpoint, data) = match serial::parse(bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                log::warn!("ESP-Hosted RPC message dropped: {error}");
                return;
            }
        };
        let message = match Message::decode(data) {
            Ok(message) => message,
            Err(error) => {
                log::warn!("ESP-Hosted RPC message dropped: {error}");
                return;
            }
        };
        match (endpoint, message.kind) {
            (_, MessageKind::Response) => self.shared.response.signal(data.to_vec()),
            (Endpoint::Event, _) | (_, MessageKind::Event) => match Event::decode(&message) {
                Ok(event) => self.handle_event(event),
                Err(error) => log::warn!("ESP-Hosted event dropped: {error}"),
            },
            (_, MessageKind::Request) => {}
        }
    }

    fn handle_event(&mut self, event: Event) {
        match &event {
            Event::StationConnected { ssid, channel, .. } => {
                log::info!("Wi-Fi station associated with {ssid:?} on channel {channel}");
                self.shared.station_connected.lock(|cell| cell.set(true));
                self.station.set_link_state(LinkState::Up);
            }
            Event::StationDisconnected { reason, .. } => {
                if self.shared.station_connected() {
                    log::info!("Wi-Fi station disconnected (reason {reason})");
                }
                self.shared.station_connected.lock(|cell| cell.set(false));
                self.station.set_link_state(LinkState::Down);
            }
            Event::Wifi(rpc::wifi_event::AP_START) => {
                self.access_point.set_link_state(LinkState::Up);
            }
            Event::Wifi(rpc::wifi_event::AP_STOP) => {
                self.access_point.set_link_state(LinkState::Down);
            }
            _ => {}
        }
        self.shared.publish(event);
    }
}

fn link_aggregated(mode: Result<SdioMode, crate::init::Incompatible>) -> bool {
    matches!(mode, Ok(SdioMode::Aggregated { .. }))
}

fn deliver(runner: &mut ch::Runner<'_, MTU>, frame: &[u8]) {
    if frame.len() > MTU {
        log::warn!("ESP-Hosted frame of {} bytes exceeds the MTU", frame.len());
        return;
    }
    match runner.try_rx_buf() {
        Some(buffer) => {
            buffer[..frame.len()].copy_from_slice(frame);
            runner.rx_done(frame.len());
        }
        None => log::debug!("ESP-Hosted frame dropped: the IP stack is busy"),
    }
}

/// Frames `payload` for `interface` and writes it once the co-processor has
/// room.
async fn send<B: MmcBus, D: DelayNs>(
    link: &mut Link<B, D>,
    buffer: &mut Aligned<A4, [u8; TX_TRANSFER]>,
    interface: Interface,
    payload: &[u8],
) -> Result<(), TransportError> {
    let header = Header::new(interface, 0, link.sequence);
    link.sequence = link.sequence.wrapping_add(1);
    let length = frame::encode_packet(header, payload, &mut buffer[..])
        .map_err(|_| TransportError::TooLarge)?;
    let (transfer, credits) = match link.aggregate_credit {
        // ESP-Hosted 3.x software aggregation: a 4-byte aligned aggregate of
        // one packet, one credit per co-processor buffer it spans.
        Some(unit) => {
            let aligned = (length + 3) & !3;
            buffer[length..aligned].fill(0);
            (aligned, (aligned as u32).div_ceil(unit))
        }
        None => (length, (length as u32).div_ceil(CREDIT_BYTES)),
    };
    let mut polls = 0;
    while !link.transport.take_credits(credits).await? {
        polls += 1;
        if polls >= CREDIT_POLLS {
            log::warn!(
                "ESP-Hosted co-processor has no receive buffer; {interface:?} packet dropped"
            );
            return Ok(());
        }
        Timer::after(Duration::from_millis(u64::from(polls.min(30)))).await;
    }
    link.transport
        .write(aligned(buffer, transfer), credits)
        .await
}
