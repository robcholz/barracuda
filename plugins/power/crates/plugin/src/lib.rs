//! The Board's power monitor: its readings and the battery's charge, for
//! every Plugin that needs them.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{boxed::Box, format, rc::Rc, string::String};
use barracuda_board_hal::power::{PowerMonitor, PowerMonitorPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginRegisterContext, PluginResult};
use core::cell::RefCell;
use portable_atomic_util::Arc;

pub use barracuda_board_hal::power::{BatteryState, PowerMeasurement};

/// Why a reading failed: the Board has no power monitor, or its driver's
/// description of the failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerError(pub String);

/// A power monitor behind a lock, read from any task.
trait Source: Send + Sync {
    fn measure(&self) -> Result<PowerMeasurement, PowerError>;
    fn battery(&self) -> Option<BatteryState>;
}

struct Locked<Monitor>(critical_section::Mutex<RefCell<Monitor>>);

impl<Monitor> Source for Locked<Monitor>
where
    Monitor: PowerMonitor + Send,
{
    fn measure(&self) -> Result<PowerMeasurement, PowerError> {
        critical_section::with(|section| {
            self.0
                .borrow(section)
                .borrow_mut()
                .measure()
                .map_err(|error| PowerError(format!("{error:?}")))
        })
    }

    fn battery(&self) -> Option<BatteryState> {
        critical_section::with(|section| {
            self.0.borrow(section).borrow_mut().battery().ok().flatten()
        })
    }
}

/// The Board's power monitor, published by the `power` Plugin. Cloning it
/// is cheap, and a clone may be kept in any task, such as a Lua package's;
/// every clone reads the same monitor.
#[derive(Clone)]
pub struct Power {
    source: Option<Arc<Box<dyn Source>>>,
}

impl Power {
    /// Shares `monitor`; `None` for a Board without one.
    #[must_use]
    pub fn from_monitor<Monitor>(monitor: Option<Monitor>) -> Self
    where
        Monitor: PowerMonitor + Send + 'static,
    {
        Self {
            source: monitor.map(|monitor| {
                Arc::new(
                    Box::new(Locked(critical_section::Mutex::new(RefCell::new(monitor))))
                        as Box<dyn Source>,
                )
            }),
        }
    }

    /// Whether the Board has a power monitor.
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.source.is_some()
    }

    /// Reads one coherent electrical measurement.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError`] without a monitor or when the reading fails.
    pub fn measure(&self) -> Result<PowerMeasurement, PowerError> {
        self.source
            .as_ref()
            .ok_or_else(|| PowerError(String::from("no power monitor")))?
            .measure()
    }

    /// Reads the battery's charge now; `None` on a Board without a battery
    /// fuel gauge or when the reading fails.
    #[must_use]
    pub fn battery(&self) -> Option<BatteryState> {
        self.source.as_ref()?.battery()
    }
}

/// Owns the Board's primary power monitor and publishes it as [`Power`].
#[barracuda_plugin::macros::plugin]
pub struct PowerPlugin<Monitor> {
    monitor: Option<Monitor>,
}

impl<Monitor> PowerPlugin<Monitor> {
    /// Takes the built-in power monitor from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: PowerMonitorPeripheral<PowerMonitor = Monitor>,
    {
        Self {
            monitor: config::peripherals::POWER_MONITOR_DEVICE
                .and_then(|instance| context.hal.peripherals.take_power_monitor(instance)),
        }
    }
}

impl<Monitor> Plugin for PowerPlugin<Monitor>
where
    Monitor: PowerMonitor + Send + 'static,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        context.provide(Rc::new(Power::from_monitor(self.monitor.take())))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[derive(Debug)]
    struct BusFault;

    struct Gauge;

    impl PowerMonitor for Gauge {
        type Error = BusFault;

        fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
            Err(BusFault)
        }

        fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
            Ok(Some(BatteryState {
                percent: 76,
                charging: true,
            }))
        }
    }

    #[test]
    fn clones_read_the_same_monitor() {
        let power = Power::from_monitor(Some(Gauge));
        let clone = power.clone();
        assert!(clone.is_present());
        assert_eq!(
            clone.battery(),
            Some(BatteryState {
                percent: 76,
                charging: true,
            })
        );
        assert_eq!(power.measure(), Err(PowerError(String::from("BusFault"))));
        let none = Power::from_monitor(None::<Gauge>);
        assert!(!none.is_present());
        assert_eq!(none.battery(), None);
        assert!(none.measure().is_err());
    }
}
