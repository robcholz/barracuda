//! CAN bus implementation over an on-chip CAN controller.
//!
//! The Platform's controller (the ESP TWAI) does the bus work; a transceiver
//! needs no driver. This implementation keeps the controller off the bus
//! until an application chooses the bit rate, remembers the rate while
//! started, and refuses transfers while stopped.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::CanController;
use barracuda_peripheral::{
    PeripheralImplementation,
    can::{BusState, Can, Frame},
};

/// Board-owned configuration; the controller binding carries everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TwaiCanConfig;

impl TwaiCanConfig {
    /// Creates the configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// CAN failure.
#[derive(Debug, PartialEq, Eq)]
pub enum TwaiCanError<ControllerError> {
    /// The controller is not on the bus; start it first.
    NotStarted,
    /// The controller cannot run at this bit rate.
    UnsupportedBitrate(u32),
    /// The controller failed a transfer or left the bus.
    Controller(ControllerError),
}

/// Initialized CAN bus.
pub struct TwaiCan<C> {
    controller: C,
    bitrate: Option<u32>,
}

/// Static factory used by generated Board composition.
pub struct TwaiCanImplementation<C>(PhantomData<fn() -> C>);

impl<C> PeripheralImplementation for TwaiCanImplementation<C>
where
    C: CanController + 'static,
{
    type Bindings = C;
    type Config = TwaiCanConfig;
    type Peripheral = TwaiCan<C>;
    type Error = core::convert::Infallible;

    async fn initialize(
        controller: C,
        _config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ok(TwaiCan {
            controller,
            bitrate: None,
        })
    }
}

impl<C: CanController> TwaiCan<C> {
    fn ensure_started(&self) -> Result<(), TwaiCanError<C::Error>> {
        if self.bitrate.is_some() {
            Ok(())
        } else {
            Err(TwaiCanError::NotStarted)
        }
    }
}

impl<C: CanController> Can for TwaiCan<C> {
    type Error = TwaiCanError<C::Error>;

    fn supports_bitrate(&self, bitrate: u32) -> bool {
        self.controller.supports_bitrate(bitrate)
    }

    fn start(&mut self, bitrate: u32) -> Result<(), Self::Error> {
        if !self.controller.supports_bitrate(bitrate) {
            return Err(TwaiCanError::UnsupportedBitrate(bitrate));
        }
        self.bitrate = None;
        self.controller
            .start(bitrate)
            .map_err(TwaiCanError::Controller)?;
        self.bitrate = Some(bitrate);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.controller.stop();
        self.bitrate = None;
        Ok(())
    }

    fn bitrate(&self) -> Option<u32> {
        self.bitrate
    }

    fn state(&self) -> BusState {
        if self.bitrate.is_none() {
            BusState::Stopped
        } else {
            self.controller.state()
        }
    }

    async fn transmit(&mut self, frame: &Frame) -> Result<(), Self::Error> {
        self.ensure_started()?;
        self.controller
            .transmit(frame)
            .await
            .map_err(TwaiCanError::Controller)
    }

    async fn receive(&mut self) -> Result<Frame, Self::Error> {
        self.ensure_started()?;
        self.controller
            .receive()
            .await
            .map_err(TwaiCanError::Controller)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use std::vec::Vec;

    use barracuda_board_hal::CanController;
    use barracuda_peripheral::{
        PeripheralImplementation,
        can::{BusState, Can, Frame, Id},
    };
    use embassy_futures::block_on;

    use super::{TwaiCanConfig, TwaiCanError, TwaiCanImplementation};

    /// A controller that echoes every frame it sends back to the receiver.
    #[derive(Default)]
    struct Loopback {
        started: Option<u32>,
        queue: Vec<Frame>,
    }

    impl CanController for Loopback {
        type Error = &'static str;

        fn supports_bitrate(&self, bitrate: u32) -> bool {
            matches!(bitrate, 125_000 | 250_000 | 500_000 | 1_000_000)
        }

        fn start(&mut self, bitrate: u32) -> Result<(), Self::Error> {
            self.started = Some(bitrate);
            self.queue.clear();
            Ok(())
        }

        fn stop(&mut self) {
            self.started = None;
        }

        fn state(&self) -> BusState {
            BusState::ErrorActive
        }

        async fn transmit(&mut self, frame: &Frame) -> Result<(), Self::Error> {
            self.queue.push(*frame);
            Ok(())
        }

        async fn receive(&mut self) -> Result<Frame, Self::Error> {
            if self.queue.is_empty() {
                Err("no frame")
            } else {
                Ok(self.queue.remove(0))
            }
        }
    }

    #[test]
    fn stays_off_the_bus_until_started() {
        let mut can = block_on(TwaiCanImplementation::initialize(
            Loopback::default(),
            TwaiCanConfig::new(),
        ))
        .expect("initialize");
        assert_eq!(can.state(), BusState::Stopped);
        assert_eq!(can.bitrate(), None);
        let frame = Frame::new(Id::standard(0x123).expect("id"), &[1, 2]).expect("frame");
        assert_eq!(
            block_on(can.transmit(&frame)),
            Err(TwaiCanError::NotStarted)
        );
        assert_eq!(
            can.start(83_333),
            Err(TwaiCanError::UnsupportedBitrate(83_333))
        );
        assert_eq!(can.controller.started, None);
    }

    #[test]
    fn sends_and_receives_standard_extended_and_remote_frames() {
        let mut can = block_on(TwaiCanImplementation::initialize(
            Loopback::default(),
            TwaiCanConfig::new(),
        ))
        .expect("initialize");
        can.start(500_000).expect("start");
        assert_eq!(can.bitrate(), Some(500_000));
        assert_eq!(can.state(), BusState::ErrorActive);
        let frames = [
            Frame::new(Id::standard(0x7ff).expect("id"), &[0xde, 0xad]).expect("frame"),
            Frame::new(
                Id::extended(0x1fff_ffff).expect("id"),
                &[1, 2, 3, 4, 5, 6, 7, 8],
            )
            .expect("frame"),
            Frame::remote(Id::standard(0x10).expect("id"), 4).expect("frame"),
        ];
        for frame in &frames {
            block_on(can.transmit(frame)).expect("transmit");
        }
        for frame in &frames {
            assert_eq!(block_on(can.receive()).expect("receive"), *frame);
        }
        can.stop().expect("stop");
        assert_eq!(can.controller.started, None);
        assert_eq!(can.state(), BusState::Stopped);
    }

    #[test]
    fn frames_refuse_out_of_range_identifiers_and_lengths() {
        assert!(Id::standard(0x800).is_none());
        assert!(Id::extended(0x2000_0000).is_none());
        assert!(Frame::new(Id::Standard(1), &[0; 9]).is_none());
        assert!(Frame::remote(Id::Standard(1), 9).is_none());
        let remote = Frame::remote(Id::Standard(1), 3).expect("frame");
        assert!(remote.data().is_empty());
        assert_eq!(remote.dlc(), 3);
    }
}
