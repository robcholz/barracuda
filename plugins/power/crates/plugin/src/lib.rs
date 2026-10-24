//! The Board's power monitors: their readings and the battery's charge, for
//! every Plugin that needs them.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use barracuda_board_hal::power::{PowerMonitor, PowerMonitorPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginRegisterContext, PluginResult};
use core::cell::RefCell;
use portable_atomic_util::Arc;

pub use barracuda_board_hal::power::{BatteryState, PowerMeasurement};

/// Why a reading failed: the Board has no such power monitor, or its
/// driver's description of the failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerError(pub String);

/// A power monitor behind a lock, read from any task.
trait Source: Send + Sync {
    fn measure(&self) -> Result<PowerMeasurement, PowerError>;
    fn battery(&self) -> Option<BatteryState>;
}

struct Locked<Device>(critical_section::Mutex<RefCell<Device>>);

impl<Device> Source for Locked<Device>
where
    Device: PowerMonitor + Send,
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

/// Every monitor by its Board name, the primary one first.
type Sources = Arc<Vec<(&'static str, Box<dyn Source>)>>;

/// The Board's power monitors, published by the `power` Plugin. Cloning it
/// is cheap, and a clone may be kept in any task, such as a Lua package's;
/// every clone reads the same monitors.
///
/// The primary monitor, normally the battery's, comes first; [`measure`]
/// and [`battery`] read it, and [`monitor`] reaches any monitor by its Board
/// name.
///
/// [`measure`]: Self::measure
/// [`battery`]: Self::battery
/// [`monitor`]: Self::monitor
#[derive(Clone)]
pub struct Power {
    monitors: Sources,
}

impl Power {
    /// Shares `monitors`, each with its Board name, the primary one first;
    /// none for a Board without a power monitor.
    #[must_use]
    pub fn from_monitors<Device>(monitors: impl IntoIterator<Item = (&'static str, Device)>) -> Self
    where
        Device: PowerMonitor + Send + 'static,
    {
        Self {
            monitors: Arc::new(
                monitors
                    .into_iter()
                    .map(|(name, device)| {
                        (
                            name,
                            Box::new(Locked(critical_section::Mutex::new(RefCell::new(device))))
                                as Box<dyn Source>,
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// Whether the Board has a power monitor.
    #[must_use]
    pub fn is_present(&self) -> bool {
        !self.monitors.is_empty()
    }

    /// The Board names of the monitors, the primary one first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.monitors.iter().map(|(name, _)| *name)
    }

    /// The monitor named `name`, or the primary one when `name` is `None`;
    /// `None` when the Board has no such monitor.
    #[must_use]
    pub fn monitor(&self, name: Option<&str>) -> Option<Monitor> {
        let index = match name {
            None => (!self.monitors.is_empty()).then_some(0)?,
            Some(name) => self
                .monitors
                .iter()
                .position(|(monitor, _)| *monitor == name)?,
        };
        Some(Monitor {
            monitors: Arc::clone(&self.monitors),
            index,
        })
    }

    /// Reads one coherent electrical measurement from the primary monitor.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError`] without a monitor or when the reading fails.
    pub fn measure(&self) -> Result<PowerMeasurement, PowerError> {
        self.monitor(None)
            .ok_or_else(|| PowerError(String::from("no power monitor")))?
            .measure()
    }

    /// Reads the battery's charge from the primary monitor now; `None` on a
    /// Board without a battery fuel gauge or when the reading fails.
    #[must_use]
    pub fn battery(&self) -> Option<BatteryState> {
        self.monitor(None)?.battery()
    }
}

/// One of the Board's power monitors, from [`Power::monitor`]. Cloning it is
/// cheap, and every clone reads the same monitor.
#[derive(Clone)]
pub struct Monitor {
    monitors: Sources,
    index: usize,
}

impl Monitor {
    /// The monitor's Board name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.monitors.get(self.index).map_or("", |(name, _)| *name)
    }

    /// Reads one coherent electrical measurement.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError`] when the reading fails.
    pub fn measure(&self) -> Result<PowerMeasurement, PowerError> {
        self.monitors
            .get(self.index)
            .ok_or_else(|| PowerError(String::from("no power monitor")))?
            .1
            .measure()
    }

    /// Reads the battery's charge now; `None` from a monitor that is no
    /// battery fuel gauge or when the reading fails.
    #[must_use]
    pub fn battery(&self) -> Option<BatteryState> {
        self.monitors.get(self.index)?.1.battery()
    }
}

/// Owns every power monitor of the Board no other Plugin claims and
/// publishes them as [`Power`].
#[barracuda_plugin::macros::plugin]
pub struct PowerPlugin<Device> {
    monitors: Vec<(&'static str, Device)>,
}

impl<Device> PowerPlugin<Device> {
    /// Takes the built-in power monitors from the generated Board capability
    /// set, the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: PowerMonitorPeripheral<PowerMonitor = Device>,
    {
        Self {
            monitors: config::peripherals::POWER_MONITOR_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_power_monitor(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for PowerPlugin<Device>
where
    Device: PowerMonitor + Send + 'static,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        context.provide(Rc::new(Power::from_monitors(core::mem::take(
            &mut self.monitors,
        ))))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;

    #[derive(Debug)]
    struct BusFault;

    /// A fuel gauge whose measurement fails, or a port monitor reading
    /// `current_microamps`.
    enum TestMonitor {
        Gauge,
        Port(i64),
    }

    impl PowerMonitor for TestMonitor {
        type Error = BusFault;

        fn measure(&mut self) -> Result<PowerMeasurement, Self::Error> {
            match self {
                Self::Gauge => Err(BusFault),
                Self::Port(current_microamps) => Ok(PowerMeasurement {
                    bus_microvolts: 5_000_000,
                    shunt_nanovolts: 0,
                    current_microamps: *current_microamps,
                    power_microwatts: 0,
                }),
            }
        }

        fn battery(&mut self) -> Result<Option<BatteryState>, Self::Error> {
            Ok(matches!(self, Self::Gauge).then_some(BatteryState {
                percent: 76,
                charging: true,
            }))
        }
    }

    #[test]
    fn clones_read_the_same_primary_monitor() {
        let power = Power::from_monitors([("battery", TestMonitor::Gauge)]);
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
        let none = Power::from_monitors(std::vec::Vec::<(&str, TestMonitor)>::new());
        assert!(!none.is_present());
        assert_eq!(none.battery(), None);
        assert!(none.measure().is_err());
        assert!(none.monitor(None).is_none());
    }

    #[test]
    fn every_monitor_reads_by_name_the_primary_by_default() {
        let power = Power::from_monitors([
            ("battery", TestMonitor::Gauge),
            ("port-a1", TestMonitor::Port(120_000)),
            ("port-b1", TestMonitor::Port(-3_000)),
        ]);
        assert_eq!(
            power.names().collect::<std::vec::Vec<_>>(),
            ["battery", "port-a1", "port-b1"]
        );
        assert_eq!(
            power.monitor(None).map(|monitor| monitor.name()),
            Some("battery")
        );
        let port = power.monitor(Some("port-b1")).expect("port monitor");
        assert_eq!(port.name(), "port-b1");
        assert_eq!(port.measure().map(|m| m.current_microamps), Ok(-3_000));
        assert_eq!(port.battery(), None);
        assert!(power.monitor(Some("port-c1")).is_none());
        // The primary stays the battery for every existing reader.
        assert_eq!(power.battery().map(|battery| battery.percent), Some(76));
    }
}
