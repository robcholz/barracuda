//! Infrared remote control over pulse-train channels.
//!
//! The transmit channel clocks marks and spaces in 1 µs ticks and modulates
//! the marks with the carrier itself, so the LED is lit only in carrier
//! bursts. NEC frames come from `barracuda-driver-ir-nec`; raw pulse trains
//! are sent as given, each level longer than one symbol half split over
//! several. The receive channel records a demodulating receiver's output
//! until it rests for 12 ms, longer than any NEC mark or space, and ignores
//! glitches shorter than 3 µs.
//!
//! Symbol buffers are allocated once at start: a send or a receive allocates
//! nothing.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::marker::PhantomData;

use barracuda_board_hal::{
    PulseCarrier, PulseInput, PulseInputConfig, PulseLevel, PulseOutput, PulseOutputConfig,
};
use barracuda_driver_ir_nec::{Burst, FRAME_BURSTS, Frame};
use barracuda_peripheral::{
    PeripheralImplementation,
    infrared::{Carrier, Infrared, NecCommand, Received},
};

/// Tick frequency of both directions.
const TICK_HZ: u32 = 1_000_000;
/// Symbols one send may take: 512 levels, enough for air-conditioner remotes.
const TRANSMIT_SYMBOLS: usize = 256;
/// Symbols one reception records.
const RECEIVE_SYMBOLS: usize = 128;
/// A level this long ends a reception.
const IDLE_TICKS: u16 = 12_000;
/// Pulses shorter than this are glitches.
const FILTER_NS: u32 = 3_000;

/// Electrical level of a mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActiveLevel {
    /// A mark pulls the line low.
    Low,
    /// A mark drives the line high.
    #[default]
    High,
}

impl ActiveLevel {
    const fn is_high(self) -> bool {
        matches!(self, Self::High)
    }
}

/// Board-owned line polarities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RmtInfraredConfig {
    transmit: ActiveLevel,
    receive: ActiveLevel,
}

impl RmtInfraredConfig {
    /// Creates configuration from the transmitter's and receiver's mark levels.
    #[must_use]
    pub const fn new(transmit: ActiveLevel, receive: ActiveLevel) -> Self {
        Self { transmit, receive }
    }
}

/// Move-only resources consumed by the implementation.
pub struct RmtInfraredBindings<TX, RX> {
    transmit: TX,
    receive: Option<RX>,
}

impl<TX, RX> RmtInfraredBindings<TX, RX> {
    /// Wraps the transmit channel and the optional receive channel.
    #[must_use]
    pub const fn new(transmit: TX, receive: Option<RX>) -> Self {
        Self { transmit, receive }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum RmtInfraredInitError<OutputError, InputError> {
    /// The transmit channel rejected its timing.
    Transmit(OutputError),
    /// The receive channel rejected its timing.
    Receive(InputError),
}

/// Transfer failure.
#[derive(Debug)]
pub enum RmtInfraredError<OutputError, InputError> {
    /// The Board has no infrared receiver.
    ReceiveUnsupported,
    /// The pulse train needs more symbols than one send holds.
    TooLong,
    /// The transmit channel failed or rejected the carrier.
    Transmit(OutputError),
    /// The receive channel failed.
    Receive(InputError),
}

/// Initialized infrared transmitter and optional receiver.
pub struct RmtInfrared<TX: PulseOutput, RX: PulseInput> {
    transmit: TX,
    receive: Option<RX>,
    config: RmtInfraredConfig,
    carrier: Option<Carrier>,
    transmit_symbols: Vec<TX::Symbol>,
    receive_symbols: Vec<RX::Symbol>,
}

/// Static factory used by generated Board composition.
pub struct RmtInfraredImplementation<TX, RX>(PhantomData<fn() -> (TX, RX)>);

impl<TX, RX> PeripheralImplementation for RmtInfraredImplementation<TX, RX>
where
    TX: PulseOutput + 'static,
    RX: PulseInput + 'static,
{
    type Bindings = RmtInfraredBindings<TX, RX>;
    type Config = RmtInfraredConfig;
    type Peripheral = RmtInfrared<TX, RX>;
    type Error = RmtInfraredInitError<TX::Error, RX::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        RmtInfrared::new(bindings, config)
    }
}

type InfraredResult<T, TX, RX> =
    Result<T, RmtInfraredError<<TX as PulseOutput>::Error, <RX as PulseInput>::Error>>;

impl<TX, RX> RmtInfrared<TX, RX>
where
    TX: PulseOutput,
    RX: PulseInput,
{
    fn new(
        bindings: RmtInfraredBindings<TX, RX>,
        config: RmtInfraredConfig,
    ) -> Result<Self, RmtInfraredInitError<TX::Error, RX::Error>> {
        let mut transmit = bindings.transmit;
        transmit
            .configure(PulseOutputConfig {
                tick_hz: TICK_HZ,
                idle_high: !config.transmit.is_high(),
                carrier: None,
            })
            .map_err(RmtInfraredInitError::Transmit)?;
        let mut receive = bindings.receive;
        if let Some(input) = receive.as_mut() {
            input
                .configure(PulseInputConfig {
                    tick_hz: TICK_HZ,
                    idle_ticks: IDLE_TICKS,
                    filter_ns: FILTER_NS,
                })
                .map_err(RmtInfraredInitError::Receive)?;
        }
        let receive_symbols = if receive.is_some() {
            let mut symbols = Vec::with_capacity(RECEIVE_SYMBOLS);
            symbols.resize(RECEIVE_SYMBOLS, RX::empty_symbol());
            symbols
        } else {
            Vec::new()
        };
        Ok(Self {
            transmit,
            receive,
            config,
            carrier: None,
            transmit_symbols: Vec::with_capacity(TRANSMIT_SYMBOLS),
            receive_symbols,
        })
    }

    /// Applies `carrier` to the transmit channel when it differs from the
    /// last one sent.
    fn use_carrier(&mut self, carrier: Carrier) -> InfraredResult<(), TX, RX> {
        if self.carrier == Some(carrier) {
            return Ok(());
        }
        self.carrier = None;
        self.transmit
            .configure(PulseOutputConfig {
                tick_hz: TICK_HZ,
                idle_high: !self.config.transmit.is_high(),
                carrier: Some(PulseCarrier {
                    frequency_hz: carrier.frequency_hz(),
                    duty_percent: carrier.duty_percent(),
                }),
            })
            .map_err(RmtInfraredError::Transmit)?;
        self.carrier = Some(carrier);
        Ok(())
    }

    /// Fills the transmit buffer from timings alternating mark and space,
    /// mark first, splitting long levels and skipping empty ones.
    fn encode(&mut self, timings_us: impl Iterator<Item = u32>) -> InfraredResult<(), TX, RX> {
        let active = self.config.transmit.is_high();
        self.transmit_symbols.clear();
        let mut pending: Option<PulseLevel> = None;
        for (index, duration) in timings_us.enumerate() {
            let high = if index.is_multiple_of(2) {
                active
            } else {
                !active
            };
            let mut remaining = duration;
            while remaining != 0 {
                let ticks = remaining.min(u32::from(PulseLevel::MAX_TICKS));
                remaining = remaining.saturating_sub(ticks);
                let half = PulseLevel::new(high, u16::try_from(ticks).unwrap_or(u16::MAX));
                match pending.take() {
                    None => pending = Some(half),
                    Some(first) => self.push(TX::symbol(first, half))?,
                }
            }
        }
        let last = match pending {
            // A zero-length second half ends the transmission after the first.
            Some(first) => TX::symbol(first, PulseLevel::new(!active, 0)),
            None => TX::end_marker(),
        };
        self.push(last)
    }

    fn push(&mut self, symbol: TX::Symbol) -> InfraredResult<(), TX, RX> {
        if self.transmit_symbols.len() >= TRANSMIT_SYMBOLS {
            return Err(RmtInfraredError::TooLong);
        }
        self.transmit_symbols.push(symbol);
        Ok(())
    }

    async fn send(&mut self) -> InfraredResult<(), TX, RX> {
        self.transmit
            .transmit(&self.transmit_symbols)
            .await
            .map_err(RmtInfraredError::Transmit)
    }
}

/// Calls `emit` with each recorded level's length in microseconds, merged
/// across symbol halves, alternating mark and space from the first mark to
/// the last.
fn recorded_levels<RX: PulseInput>(
    symbols: &[RX::Symbol],
    mark_high: bool,
    mut emit: impl FnMut(u32),
) {
    let mut current: Option<(bool, u32)> = None;
    let halves = symbols.iter().flat_map(|symbol| {
        let (first, second) = RX::levels(*symbol);
        [first, second]
    });
    for half in halves.take_while(|half| half.ticks != 0) {
        let mark = half.high == mark_high;
        current = match current {
            // Leading idle before the first mark is not part of the frame.
            None if !mark => None,
            None => Some((mark, u32::from(half.ticks))),
            Some((level, length)) if level == mark => {
                Some((level, length.saturating_add(u32::from(half.ticks))))
            }
            Some((_, length)) => {
                emit(length);
                Some((mark, u32::from(half.ticks)))
            }
        };
    }
    // A trailing space is the idle gap that ended the reception.
    if let Some((true, length)) = current {
        emit(length);
    }
}

impl<TX, RX> Infrared for RmtInfrared<TX, RX>
where
    TX: PulseOutput,
    RX: PulseInput,
{
    type Error = RmtInfraredError<TX::Error, RX::Error>;

    fn can_receive(&self) -> bool {
        self.receive.is_some()
    }

    async fn send_nec(&mut self, command: NecCommand) -> Result<(), Self::Error> {
        let frame = if command.is_extended() {
            barracuda_driver_ir_nec::encode_extended(command.address(), command.command())
        } else {
            barracuda_driver_ir_nec::encode_standard(
                u8::try_from(command.address()).unwrap_or(u8::MAX),
                command.command(),
            )
        };
        self.use_carrier(Carrier::NEC)?;
        self.encode(
            frame
                .iter()
                .flat_map(|burst| [u32::from(burst.mark_us), u32::from(burst.space_us)]),
        )?;
        self.send().await
    }

    async fn send_raw(&mut self, carrier: Carrier, timings_us: &[u16]) -> Result<(), Self::Error> {
        self.use_carrier(carrier)?;
        self.encode(timings_us.iter().map(|timing| u32::from(*timing)))?;
        self.send().await
    }

    async fn receive(&mut self, timings_us: &mut [u16]) -> Result<Received, Self::Error> {
        let mark_high = self.config.receive.is_high();
        let input = self
            .receive
            .as_mut()
            .ok_or(RmtInfraredError::ReceiveUnsupported)?;
        loop {
            self.receive_symbols.fill(RX::empty_symbol());
            let count = input
                .receive(&mut self.receive_symbols)
                .await
                .map_err(RmtInfraredError::Receive)?;
            let recorded = self.receive_symbols.get(..count).unwrap_or_default();
            let mut bursts = [Burst::default(); FRAME_BURSTS];
            let mut levels = 0usize;
            recorded_levels::<RX>(recorded, mark_high, |length| {
                let micros = u16::try_from(length).unwrap_or(u16::MAX);
                if let Some(slot) = timings_us.get_mut(levels) {
                    *slot = micros;
                }
                if let Some(burst) = bursts.get_mut(levels / 2) {
                    if levels.is_multiple_of(2) {
                        burst.mark_us = micros;
                    } else {
                        burst.space_us = micros;
                    }
                }
                levels = levels.saturating_add(1);
            });
            if levels == 0 {
                // Only idle level: noise the filter let through.
                continue;
            }
            let burst_count = levels.div_ceil(2).min(FRAME_BURSTS);
            let bursts = bursts.get(..burst_count).unwrap_or_default();
            return Ok(match barracuda_driver_ir_nec::decode(bursts) {
                Some(Frame::Standard { address, command }) => {
                    Received::Nec(NecCommand::new(address, command))
                }
                Some(Frame::Extended { address, command }) => {
                    Received::Nec(NecCommand::extended(address, command))
                }
                Some(Frame::Repeat) => Received::NecRepeat,
                None => Received::Raw {
                    len: levels.min(timings_us.len()),
                },
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::vec::Vec;

    use barracuda_board_hal::{
        PulseInput, PulseInputConfig, PulseLevel, PulseOutput, PulseOutputConfig,
    };
    use barracuda_peripheral::infrared::{Carrier, Infrared, NecCommand, Received};
    use embassy_futures::block_on;

    use super::{ActiveLevel, RmtInfrared, RmtInfraredBindings, RmtInfraredConfig};

    type Symbol = (PulseLevel, PulseLevel);

    #[derive(Default)]
    struct RecordingOutput {
        config: Option<PulseOutputConfig>,
        sent: Vec<Symbol>,
    }

    impl PulseOutput for RecordingOutput {
        type Symbol = Symbol;
        type Error = Infallible;

        fn symbol(first: PulseLevel, second: PulseLevel) -> Symbol {
            (first, second)
        }

        fn end_marker() -> Symbol {
            (PulseLevel::new(false, 0), PulseLevel::new(false, 0))
        }

        fn configure(&mut self, config: PulseOutputConfig) -> Result<(), Self::Error> {
            self.config = Some(config);
            Ok(())
        }

        fn transmit_blocking(&mut self, symbols: &[Symbol]) -> Result<(), Self::Error> {
            self.sent = symbols.to_vec();
            Ok(())
        }

        async fn transmit(&mut self, symbols: &[Symbol]) -> Result<(), Self::Error> {
            self.transmit_blocking(symbols)
        }
    }

    /// Replays one recorded signal per reception.
    #[derive(Default)]
    struct ReplayInput {
        signals: Vec<Vec<Symbol>>,
    }

    impl PulseInput for ReplayInput {
        type Symbol = Symbol;
        type Error = Infallible;

        fn empty_symbol() -> Symbol {
            (PulseLevel::new(false, 0), PulseLevel::new(false, 0))
        }

        fn levels(symbol: Symbol) -> (PulseLevel, PulseLevel) {
            symbol
        }

        fn capacity(&self) -> usize {
            usize::MAX
        }

        fn configure(&mut self, _config: PulseInputConfig) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn receive(&mut self, symbols: &mut [Symbol]) -> Result<usize, Self::Error> {
            let signal = self.signals.remove(0);
            for (slot, symbol) in symbols.iter_mut().zip(&signal) {
                *slot = *symbol;
            }
            Ok(signal.len().min(symbols.len()))
        }
    }

    fn device(signals: Vec<Vec<Symbol>>) -> RmtInfrared<RecordingOutput, ReplayInput> {
        RmtInfrared::new(
            RmtInfraredBindings::new(RecordingOutput::default(), Some(ReplayInput { signals })),
            RmtInfraredConfig::new(ActiveLevel::High, ActiveLevel::Low),
        )
        .expect("start infrared")
    }

    /// A receiver's view of the NEC frame: low while the carrier is seen.
    fn received_nec(address: u8, command: u8) -> Vec<Symbol> {
        barracuda_driver_ir_nec::encode_standard(address, command)
            .iter()
            .map(|burst| {
                (
                    PulseLevel::new(false, burst.mark_us),
                    PulseLevel::new(
                        true,
                        if burst.space_us == 0 {
                            0
                        } else {
                            burst.space_us
                        },
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn nec_is_sent_on_the_38_khz_carrier_lsb_first() {
        let mut infrared = device(Vec::new());
        block_on(infrared.send_nec(NecCommand::new(0x04, 0x08))).expect("send");
        let carrier = infrared
            .transmit
            .config
            .and_then(|config| config.carrier)
            .expect("carrier");
        assert_eq!((carrier.frequency_hz, carrier.duty_percent), (38_000, 33));
        let sent = &infrared.transmit.sent;
        assert_eq!(sent.len(), 34);
        assert_eq!(
            sent[0],
            (PulseLevel::new(true, 9_000), PulseLevel::new(false, 4_500))
        );
        assert_eq!(sent[3].1, PulseLevel::new(false, 1_687));
        assert_eq!(sent[33].1.ticks, 0);
    }

    #[test]
    fn long_raw_levels_split_over_symbols() {
        let mut infrared = device(Vec::new());
        block_on(infrared.send_raw(Carrier::new(40_000, 50), &[40_000, 1_000, 500])).expect("send");
        let sent = &infrared.transmit.sent;
        // 40 ms of mark is 32767 + 7233 ticks, then the space and the last mark.
        assert_eq!(sent[0].0, PulseLevel::new(true, 32_767));
        assert_eq!(sent[0].1, PulseLevel::new(true, 7_233));
        assert_eq!(sent[1].0, PulseLevel::new(false, 1_000));
        assert_eq!(sent[1].1, PulseLevel::new(true, 500));
        assert_eq!(sent[2].0.ticks, 0);
    }

    #[test]
    fn received_nec_decodes_and_other_frames_come_back_raw() {
        let raw = std::vec![
            (PulseLevel::new(true, 500), PulseLevel::new(false, 2_400)),
            (PulseLevel::new(true, 600), PulseLevel::new(false, 1_200)),
            (PulseLevel::new(true, 12_000), PulseLevel::new(true, 0)),
        ];
        let mut infrared = device(std::vec![received_nec(0x10, 0x20), raw]);
        let mut timings = [0u16; 8];
        assert_eq!(
            block_on(infrared.receive(&mut timings)).expect("nec"),
            Received::Nec(NecCommand::new(0x10, 0x20))
        );
        assert_eq!(
            block_on(infrared.receive(&mut timings)).expect("raw"),
            Received::Raw { len: 3 }
        );
        assert_eq!(&timings[..3], &[2_400, 600, 1_200]);
    }

    #[test]
    fn a_board_without_receiver_says_so() {
        let mut infrared = RmtInfrared::<RecordingOutput, ReplayInput>::new(
            RmtInfraredBindings::new(RecordingOutput::default(), None),
            RmtInfraredConfig::new(ActiveLevel::High, ActiveLevel::Low),
        )
        .expect("start infrared");
        assert!(!infrared.can_receive());
        assert!(block_on(infrared.receive(&mut [0; 4])).is_err());
    }
}
