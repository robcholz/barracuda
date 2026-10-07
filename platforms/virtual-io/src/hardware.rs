//! Shared state of one set of virtual pins and I2C buses.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use embedded_hal::i2c::{NoAcknowledgeSource, Operation};
use serde::{Deserialize, Serialize};

use crate::{
    clock::Clock,
    device::{DeviceContext, I2cDevice, RegisterRangeError, RuleViolation},
    i2c::{VirtualI2cBus, VirtualI2cError},
};

/// Number of recorded events kept; older events are dropped first.
pub const EVENT_CAPACITY: usize = 4096;

/// Highest seven-bit I2C address.
const MAX_ADDRESS: u8 = 0x7f;

/// Electrical role currently configured on a virtual pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinMode {
    /// Neither driving nor sensing.
    Disabled,
    /// Sensing the line.
    Input,
    /// Driving the line from the output latch.
    Output,
}

/// Input bias of a virtual pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinPull {
    /// No bias: an undriven input reads low.
    None,
    /// Pull-up: an undriven input reads high.
    Up,
    /// Pull-down: an undriven input reads low.
    Down,
}

/// Output driver of a virtual pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinDrive {
    /// Drives both levels.
    PushPull,
    /// Drives low and releases high; a released, undriven line reads high.
    OpenDrain,
}

/// Observable state of one virtual pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PinSnapshot {
    /// Board-visible name, or the chip name when the Board does not expose it.
    pub name: String,
    /// Chip-native name such as `GPIO0`.
    pub chip: &'static str,
    /// Function that claimed the pin, such as `digital` or `I2C0 SCL`.
    pub function: Option<String>,
    /// Configured mode.
    pub mode: PinMode,
    /// Configured input bias.
    pub pull: PinPull,
    /// Configured output driver.
    pub drive: PinDrive,
    /// Output latch.
    pub output: bool,
    /// Level driven onto the line from outside, or `None` when undriven.
    pub driven: Option<bool>,
    /// Resolved line level.
    pub level: bool,
}

/// One devices-and-controller view of a virtual I2C bus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BusSnapshot {
    /// Controller name such as `I2C0`.
    pub name: &'static str,
    /// Board name of the SCL pin while the controller is open.
    pub scl: Option<String>,
    /// Board name of the SDA pin while the controller is open.
    pub sda: Option<String>,
    /// Configured clock while the controller is open.
    pub frequency_hz: Option<u32>,
    /// Attached devices.
    pub devices: Vec<DeviceSnapshot>,
}

/// One attached device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceSnapshot {
    /// Seven-bit address.
    pub address: u8,
    /// Device model name.
    pub model: &'static str,
}

/// Failure injected into I2C transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultKind {
    /// The address is not acknowledged.
    Nack,
    /// Another controller won arbitration.
    ArbitrationLoss,
    /// The bus did not complete in time (clock stretching or a stuck line).
    Timeout,
    /// A misplaced START or STOP was detected.
    BusError,
}

impl FaultKind {
    const fn error(self) -> VirtualI2cError {
        match self {
            Self::Nack => VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Address),
            Self::ArbitrationLoss => VirtualI2cError::ArbitrationLoss,
            Self::Timeout => VirtualI2cError::Timeout,
            Self::BusError => VirtualI2cError::Bus,
        }
    }
}

/// One active fault rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FaultRule {
    /// Identifier returned when the rule was injected.
    pub id: u64,
    /// Controller name the rule applies to.
    pub bus: &'static str,
    /// Address the rule applies to, or every address when `None`.
    pub address: Option<u8>,
    /// Failure produced.
    pub kind: FaultKind,
    /// Whether the rule is removed after its first match.
    pub once: bool,
}

/// One recorded bus transaction or pin change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Event {
    /// Monotonic sequence number, unique within the hardware model.
    pub seq: u64,
    /// Microseconds on the hardware clock.
    pub at_us: u64,
    /// What happened.
    #[serde(flatten)]
    pub detail: EventDetail,
}

/// Payload of a recorded event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EventDetail {
    /// A pin changed mode or resolved level.
    Gpio {
        /// Board or chip name.
        pin: String,
        /// Mode after the change.
        mode: PinMode,
        /// Resolved level after the change.
        level: bool,
        /// `system` for HAL calls, `manager` for control requests.
        source: &'static str,
    },
    /// One I2C transaction.
    I2c {
        /// Controller name.
        bus: &'static str,
        /// Seven-bit address.
        address: u8,
        /// Phases in bus order.
        operations: Vec<OperationRecord>,
        /// `ok`, `nack`, `arbitration-loss`, `timeout`, or `bus-error`.
        result: &'static str,
        /// Whether the failure came from an injected fault.
        injected: bool,
    },
}

/// One phase of a recorded I2C transaction, as lowercase hex bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationRecord {
    /// Bytes written by the controller.
    Write(String),
    /// Bytes returned by the device.
    Read(String),
}

/// A datasheet rule violation reported by a device model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordedViolation {
    /// Sequence number of the transaction that exposed it (0 at attach).
    pub seq: u64,
    /// Microseconds on the hardware clock.
    pub at_us: u64,
    /// Controller name.
    pub bus: &'static str,
    /// Device address.
    pub address: u8,
    /// Device model name.
    pub model: &'static str,
    /// Rule and observation.
    #[serde(flatten)]
    pub violation: RuleViolation,
}

/// Invalid request against the virtual hardware.
#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    /// No pin has this Board or chip name.
    #[error("unknown pin `{0}`")]
    UnknownPin(String),
    /// No controller has this name.
    #[error("unknown I2C bus `{0}`")]
    UnknownBus(String),
    /// The address is not a seven-bit address.
    #[error("I2C address {0} is not a seven-bit address")]
    InvalidAddress(u64),
    /// A device already answers the address.
    #[error("{bus} already has a device at address {address:#04x}")]
    AddressInUse {
        /// Controller name.
        bus: &'static str,
        /// Seven-bit address.
        address: u8,
    },
    /// No device answers the address.
    #[error("{bus} has no device at address {address:#04x}")]
    NoDevice {
        /// Controller name.
        bus: &'static str,
        /// Seven-bit address.
        address: u8,
    },
    /// Backdoor register access out of range.
    #[error(transparent)]
    Register(#[from] RegisterRangeError),
}

#[derive(Debug)]
struct PinState {
    chip: &'static str,
    board: Option<&'static str>,
    function: Option<String>,
    mode: PinMode,
    pull: PinPull,
    drive: PinDrive,
    output: bool,
    driven: Option<bool>,
}

impl PinState {
    const fn new(chip: &'static str) -> Self {
        Self {
            chip,
            board: None,
            function: None,
            mode: PinMode::Disabled,
            pull: PinPull::None,
            drive: PinDrive::PushPull,
            output: false,
            driven: None,
        }
    }

    fn name(&self) -> &'static str {
        self.board.unwrap_or(self.chip)
    }

    fn level(&self) -> bool {
        match self.mode {
            PinMode::Output => match self.drive {
                PinDrive::PushPull => self.output,
                PinDrive::OpenDrain => self.output && self.driven.unwrap_or(true),
            },
            PinMode::Input => self.driven.unwrap_or(self.pull == PinPull::Up),
            PinMode::Disabled => self.driven.unwrap_or(false),
        }
    }

    fn snapshot(&self) -> PinSnapshot {
        PinSnapshot {
            name: String::from(self.name()),
            chip: self.chip,
            function: self.function.clone(),
            mode: self.mode,
            pull: self.pull,
            drive: self.drive,
            output: self.output,
            driven: self.driven,
            level: self.level(),
        }
    }
}

struct BusState {
    name: &'static str,
    scl: Option<usize>,
    sda: Option<usize>,
    frequency_hz: Option<u32>,
    devices: BTreeMap<u8, Box<dyn I2cDevice>>,
}

struct Hardware {
    pins: Vec<PinState>,
    buses: Vec<BusState>,
    faults: Vec<FaultRule>,
    next_fault: u64,
    events: VecDeque<Event>,
    next_seq: u64,
    violations: Vec<RecordedViolation>,
}

/// Shared handle to one virtual hardware model.
///
/// The host Platform keeps one process-wide model behind its control server.
/// Driver tests create their own with [`VirtualHardware::new`] and a manual
/// [`Clock`], attach device models, and open a bus with
/// [`VirtualHardware::i2c_bus`].
#[derive(Clone)]
pub struct VirtualHardware {
    shared: Arc<Mutex<Hardware>>,
    clock: Clock,
}

impl core::fmt::Debug for VirtualHardware {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VirtualHardware")
            .finish_non_exhaustive()
    }
}

impl VirtualHardware {
    /// Creates pins and I2C controllers with the given chip-native names.
    #[must_use]
    pub fn new(pins: &[&'static str], buses: &[&'static str], clock: Clock) -> Self {
        let hardware = Hardware {
            pins: pins.iter().map(|chip| PinState::new(chip)).collect(),
            buses: buses
                .iter()
                .map(|name| BusState {
                    name,
                    scl: None,
                    sda: None,
                    frequency_hz: None,
                    devices: BTreeMap::new(),
                })
                .collect(),
            faults: Vec::new(),
            next_fault: 1,
            events: VecDeque::new(),
            next_seq: 1,
            violations: Vec::new(),
        };
        Self {
            shared: Arc::new(Mutex::new(hardware)),
            clock,
        }
    }

    /// Clock that timestamps this model's events.
    #[must_use]
    pub const fn clock(&self) -> &Clock {
        &self.clock
    }

    fn lock(&self) -> MutexGuard<'_, Hardware> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now_us(&self) -> u64 {
        micros(self.clock.now())
    }

    /// Gives a chip pin its Board-visible name.
    pub fn name_pin(&self, index: usize, board: &'static str) {
        if let Some(pin) = self.lock().pins.get_mut(index) {
            pin.board = Some(board);
        }
    }

    /// Returns every pin, in chip order.
    #[must_use]
    pub fn pins(&self) -> Vec<PinSnapshot> {
        self.lock().pins.iter().map(PinState::snapshot).collect()
    }

    /// Returns one pin by Board or chip name.
    ///
    /// # Errors
    ///
    /// Returns [`HardwareError::UnknownPin`] for an unknown name.
    pub fn pin(&self, name: &str) -> Result<PinSnapshot, HardwareError> {
        let hardware = self.lock();
        let index = pin_index(&hardware, name)?;
        Ok(hardware.pins[index].snapshot())
    }

    /// Drives a pin's line from outside, or releases it with `None`.
    ///
    /// # Errors
    ///
    /// Returns [`HardwareError::UnknownPin`] for an unknown name.
    pub fn drive(&self, name: &str, level: Option<bool>) -> Result<PinSnapshot, HardwareError> {
        let at_us = self.now_us();
        let mut hardware = self.lock();
        let index = pin_index(&hardware, name)?;
        hardware.update_pin(index, at_us, "manager", |pin| pin.driven = level);
        Ok(hardware.pins[index].snapshot())
    }

    /// Returns every controller and its attached devices.
    #[must_use]
    pub fn buses(&self) -> Vec<BusSnapshot> {
        let hardware = self.lock();
        hardware
            .buses
            .iter()
            .map(|bus| BusSnapshot {
                name: bus.name,
                scl: bus.scl.map(|pin| String::from(hardware.pins[pin].name())),
                sda: bus.sda.map(|pin| String::from(hardware.pins[pin].name())),
                frequency_hz: bus.frequency_hz,
                devices: bus
                    .devices
                    .iter()
                    .map(|(address, device)| DeviceSnapshot {
                        address: *address,
                        model: device.model(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Attaches a device model at a seven-bit address.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus, an invalid address, or an address
    /// that already answers.
    pub fn attach(
        &self,
        bus: &str,
        address: u64,
        mut device: Box<dyn I2cDevice>,
    ) -> Result<(), HardwareError> {
        let address = seven_bit(address)?;
        let now = self.clock.now();
        let mut hardware = self.lock();
        let index = bus_index(&hardware, bus)?;
        let name = hardware.buses[index].name;
        if hardware.buses[index].devices.contains_key(&address) {
            return Err(HardwareError::AddressInUse { bus: name, address });
        }
        let mut violations = Vec::new();
        device.attached(&mut DeviceContext::new(now, &mut violations));
        let model = device.model();
        hardware.buses[index].devices.insert(address, device);
        hardware.record_violations(0, micros(now), name, address, model, violations);
        Ok(())
    }

    /// Removes the device at an address.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus or an absent device.
    pub fn detach(&self, bus: &str, address: u64) -> Result<(), HardwareError> {
        let address = seven_bit(address)?;
        let mut hardware = self.lock();
        let index = bus_index(&hardware, bus)?;
        let state = &mut hardware.buses[index];
        state
            .devices
            .remove(&address)
            .map(|_device| ())
            .ok_or(HardwareError::NoDevice {
                bus: state.name,
                address,
            })
    }

    /// Reads model-defined register bytes without a bus transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus, an absent device, or a range
    /// outside the model's register file.
    pub fn read_registers(
        &self,
        bus: &str,
        address: u64,
        offset: usize,
        length: usize,
    ) -> Result<Vec<u8>, HardwareError> {
        let mut bytes = vec![0; length];
        self.with_device(bus, address, |device| device.peek(offset, &mut bytes))??;
        Ok(bytes)
    }

    /// Stores model-defined register bytes without a bus transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus, an absent device, or a range
    /// outside the model's register file.
    pub fn write_registers(
        &self,
        bus: &str,
        address: u64,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), HardwareError> {
        self.with_device(bus, address, |device| device.poke(offset, bytes))??;
        Ok(())
    }

    fn with_device<T>(
        &self,
        bus: &str,
        address: u64,
        operation: impl FnOnce(&mut dyn I2cDevice) -> T,
    ) -> Result<T, HardwareError> {
        let address = seven_bit(address)?;
        let mut hardware = self.lock();
        let index = bus_index(&hardware, bus)?;
        let state = &mut hardware.buses[index];
        let name = state.name;
        let device = state
            .devices
            .get_mut(&address)
            .ok_or(HardwareError::NoDevice { bus: name, address })?;
        Ok(operation(device.as_mut()))
    }

    /// Adds a fault rule and returns its identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus or an invalid address.
    pub fn inject_fault(
        &self,
        bus: &str,
        address: Option<u64>,
        kind: FaultKind,
        once: bool,
    ) -> Result<u64, HardwareError> {
        let address = address.map(seven_bit).transpose()?;
        let mut hardware = self.lock();
        let index = bus_index(&hardware, bus)?;
        let id = hardware.next_fault;
        hardware.next_fault += 1;
        let bus = hardware.buses[index].name;
        hardware.faults.push(FaultRule {
            id,
            bus,
            address,
            kind,
            once,
        });
        Ok(id)
    }

    /// Returns the active fault rules.
    #[must_use]
    pub fn faults(&self) -> Vec<FaultRule> {
        self.lock().faults.clone()
    }

    /// Removes fault rules on one bus, or on every bus, and returns how many.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown bus.
    pub fn clear_faults(&self, bus: Option<&str>) -> Result<usize, HardwareError> {
        let mut hardware = self.lock();
        let bus = bus
            .map(|bus| bus_index(&hardware, bus).map(|index| hardware.buses[index].name))
            .transpose()?;
        let before = hardware.faults.len();
        hardware
            .faults
            .retain(|rule| bus.is_some_and(|bus| rule.bus != bus));
        Ok(before - hardware.faults.len())
    }

    /// Returns the recorded events whose sequence number is at least `since`.
    #[must_use]
    pub fn events(&self, since: u64) -> Vec<Event> {
        self.lock()
            .events
            .iter()
            .filter(|event| event.seq >= since)
            .cloned()
            .collect()
    }

    /// Returns every datasheet rule violation reported so far.
    #[must_use]
    pub fn violations(&self) -> Vec<RecordedViolation> {
        self.lock().violations.clone()
    }

    /// Removes devices, faults, external drives, events, and violations.
    ///
    /// Pin modes, output latches, and controller claims belong to the System
    /// and are kept.
    pub fn reset(&self) {
        let mut hardware = self.lock();
        for bus in &mut hardware.buses {
            bus.devices.clear();
        }
        for pin in &mut hardware.pins {
            pin.driven = None;
        }
        hardware.faults.clear();
        hardware.events.clear();
        hardware.violations.clear();
    }

    /// Opens a controller for a driver test, without pin routing.
    ///
    /// # Errors
    ///
    /// Returns [`HardwareError::UnknownBus`] for an unknown controller.
    pub fn i2c_bus(&self, bus: &str) -> Result<VirtualI2cBus, HardwareError> {
        let index = bus_index(&self.lock(), bus)?;
        Ok(VirtualI2cBus::new(self.clone(), index))
    }

    pub(crate) fn claim_digital(&self, pin: usize) {
        if let Some(pin) = self.lock().pins.get_mut(pin) {
            pin.function = Some(String::from("digital"));
        }
    }

    pub(crate) fn claim_i2c(&self, bus: usize, scl: usize, sda: usize, frequency_hz: u32) {
        let mut hardware = self.lock();
        let Some(name) = hardware.buses.get(bus).map(|bus| bus.name) else {
            return;
        };
        for (pin, role) in [(scl, "SCL"), (sda, "SDA")] {
            if let Some(pin) = hardware.pins.get_mut(pin) {
                pin.function = Some(format!("{name} {role}"));
                pin.mode = PinMode::Output;
                pin.drive = PinDrive::OpenDrain;
                pin.output = true;
            }
        }
        let state = &mut hardware.buses[bus];
        state.scl = Some(scl);
        state.sda = Some(sda);
        state.frequency_hz = Some(frequency_hz);
    }

    pub(crate) fn configure_pin(
        &self,
        pin: usize,
        mode: PinMode,
        pull: PinPull,
        drive: PinDrive,
        output: Option<bool>,
    ) {
        let at_us = self.now_us();
        self.lock().update_pin(pin, at_us, "system", |state| {
            state.mode = mode;
            state.pull = pull;
            state.drive = drive;
            if let Some(output) = output {
                state.output = output;
            }
        });
    }

    pub(crate) fn set_output(&self, pin: usize, level: bool) {
        let at_us = self.now_us();
        self.lock()
            .update_pin(pin, at_us, "system", |state| state.output = level);
    }

    pub(crate) fn level(&self, pin: usize) -> bool {
        self.lock().pins.get(pin).is_some_and(PinState::level)
    }

    pub(crate) fn output_latch(&self, pin: usize) -> bool {
        self.lock().pins.get(pin).is_some_and(|pin| pin.output)
    }

    pub(crate) fn transaction(
        &self,
        bus: usize,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), VirtualI2cError> {
        let now = self.clock.now();
        let mut hardware = self.lock();
        let Some(name) = hardware.buses.get(bus).map(|bus| bus.name) else {
            return Err(VirtualI2cError::Bus);
        };
        let seq = hardware.next_seq;
        let fault = hardware
            .faults
            .iter()
            .position(|rule| rule.bus == name && rule.address.is_none_or(|a| a == address));
        if let Some(position) = fault {
            let rule = if hardware.faults[position].once {
                hardware.faults.remove(position)
            } else {
                hardware.faults[position].clone()
            };
            let error = rule.kind.error();
            hardware.record(
                now,
                EventDetail::I2c {
                    bus: name,
                    address,
                    operations: Vec::new(),
                    result: error.label(),
                    injected: true,
                },
            );
            return Err(error);
        }
        let Some(device) = hardware.buses[bus].devices.get_mut(&address) else {
            let error = VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Address);
            hardware.record(
                now,
                EventDetail::I2c {
                    bus: name,
                    address,
                    operations: Vec::new(),
                    result: error.label(),
                    injected: false,
                },
            );
            return Err(error);
        };
        let model = device.model();
        let mut violations = Vec::new();
        let mut records = Vec::new();
        let result = run_phases(
            device.as_mut(),
            &mut DeviceContext::new(now, &mut violations),
            operations,
            &mut records,
        );
        let label = result.err().map_or("ok", VirtualI2cError::label);
        hardware.record(
            now,
            EventDetail::I2c {
                bus: name,
                address,
                operations: records,
                result: label,
                injected: false,
            },
        );
        hardware.record_violations(seq, micros(now), name, address, model, violations);
        result
    }
}

fn run_phases(
    device: &mut dyn I2cDevice,
    context: &mut DeviceContext<'_>,
    operations: &mut [Operation<'_>],
    records: &mut Vec<OperationRecord>,
) -> Result<(), VirtualI2cError> {
    let mut index = 0;
    let mut result = Ok(());
    while index < operations.len() {
        let end = index
            + operations[index..]
                .iter()
                .take_while(|operation| {
                    matches!(
                        (operation, &operations[index]),
                        (Operation::Write(_), Operation::Write(_))
                            | (Operation::Read(_), Operation::Read(_))
                    )
                })
                .count();
        let phase = &mut operations[index..end];
        if matches!(phase.first(), Some(Operation::Write(_))) {
            let mut bytes = Vec::new();
            for operation in phase.iter() {
                if let Operation::Write(written) = operation {
                    bytes.extend_from_slice(written);
                }
            }
            records.push(OperationRecord::Write(hex(&bytes)));
            if device.write(context, &bytes).is_err() {
                result = Err(VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Data));
                break;
            }
        } else {
            let length = phase
                .iter()
                .map(|operation| match operation {
                    Operation::Read(buffer) => buffer.len(),
                    Operation::Write(_) => 0,
                })
                .sum();
            let mut bytes = vec![0; length];
            device.read(context, &mut bytes);
            records.push(OperationRecord::Read(hex(&bytes)));
            let mut remaining = bytes.as_slice();
            for operation in phase.iter_mut() {
                if let Operation::Read(buffer) = operation {
                    let (head, tail) = remaining.split_at(buffer.len());
                    buffer.copy_from_slice(head);
                    remaining = tail;
                }
            }
        }
        index = end;
    }
    device.stop(context);
    result
}

impl Hardware {
    fn record(&mut self, now: Duration, detail: EventDetail) {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.events.len() == EVENT_CAPACITY {
            let _oldest = self.events.pop_front();
        }
        self.events.push_back(Event {
            seq,
            at_us: micros(now),
            detail,
        });
    }

    fn record_violations(
        &mut self,
        seq: u64,
        at_us: u64,
        bus: &'static str,
        address: u8,
        model: &'static str,
        violations: Vec<RuleViolation>,
    ) {
        for violation in violations {
            log::warn!(
                "virtual {model} at {bus}/{address:#04x} broke {}: {}",
                violation.rule,
                violation.message
            );
            self.violations.push(RecordedViolation {
                seq,
                at_us,
                bus,
                address,
                model,
                violation,
            });
        }
    }

    fn update_pin(
        &mut self,
        index: usize,
        at_us: u64,
        source: &'static str,
        change: impl FnOnce(&mut PinState),
    ) {
        let Some(pin) = self.pins.get_mut(index) else {
            return;
        };
        let before = (pin.mode, pin.level());
        change(pin);
        let after = (pin.mode, pin.level());
        if before != after {
            let detail = EventDetail::Gpio {
                pin: String::from(pin.name()),
                mode: after.0,
                level: after.1,
                source,
            };
            self.record(Duration::from_micros(at_us), detail);
        }
    }
}

fn pin_index(hardware: &Hardware, name: &str) -> Result<usize, HardwareError> {
    hardware
        .pins
        .iter()
        .position(|pin| pin.board == Some(name) || pin.chip == name)
        .ok_or_else(|| HardwareError::UnknownPin(String::from(name)))
}

fn bus_index(hardware: &Hardware, name: &str) -> Result<usize, HardwareError> {
    hardware
        .buses
        .iter()
        .position(|bus| bus.name == name)
        .ok_or_else(|| HardwareError::UnknownBus(String::from(name)))
}

fn seven_bit(address: u64) -> Result<u8, HardwareError> {
    u8::try_from(address)
        .ok()
        .filter(|address| *address <= MAX_ADDRESS)
        .ok_or(HardwareError::InvalidAddress(address))
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Encodes bytes as lowercase hex without separators.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Decodes lowercase or uppercase hex without separators.
///
/// # Errors
///
/// Returns `None` for an odd length or a non-hex digit.
#[must_use]
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::device::RegisterDevice;

    fn hardware() -> VirtualHardware {
        let hardware = VirtualHardware::new(&["GPIO0", "GPIO1"], &["I2C0"], Clock::manual());
        hardware.name_pin(0, "vio-0");
        hardware
    }

    #[test]
    fn pins_resolve_levels_from_mode_pull_latch_and_drive() {
        let hardware = hardware();
        assert!(!hardware.pin("vio-0").expect("pin").level);
        hardware.configure_pin(0, PinMode::Input, PinPull::Up, PinDrive::PushPull, None);
        assert!(hardware.level(0));
        hardware.drive("vio-0", Some(false)).expect("drive");
        assert!(!hardware.level(0));
        hardware.configure_pin(
            0,
            PinMode::Output,
            PinPull::None,
            PinDrive::PushPull,
            Some(true),
        );
        assert!(
            hardware.level(0),
            "a push-pull output ignores the external drive"
        );
        hardware.configure_pin(
            0,
            PinMode::Output,
            PinPull::None,
            PinDrive::OpenDrain,
            Some(true),
        );
        assert!(
            !hardware.level(0),
            "an external low wins on an open-drain line"
        );
        hardware.drive("GPIO0", None).expect("release");
        assert!(hardware.level(0), "a released open-drain line reads high");
    }

    #[test]
    fn pin_changes_are_recorded_with_their_source() {
        let hardware = hardware();
        hardware.configure_pin(
            0,
            PinMode::Output,
            PinPull::None,
            PinDrive::PushPull,
            Some(false),
        );
        hardware.clock().advance(Duration::from_micros(10));
        hardware.set_output(0, true);
        hardware.set_output(0, true);
        let events = hardware.events(0);
        assert_eq!(
            events.len(),
            2,
            "an unchanged level is not recorded: {events:?}"
        );
        assert_eq!(events[1].at_us, 10);
        assert_eq!(
            events[1].detail,
            EventDetail::Gpio {
                pin: String::from("vio-0"),
                mode: PinMode::Output,
                level: true,
                source: "system",
            }
        );
    }

    #[test]
    fn transactions_reach_devices_and_absent_addresses_nack() {
        let hardware = hardware();
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");
        bus.write(0x50, &[0x02, 0xaa, 0xbb]).expect("write");
        let mut read = [0; 2];
        bus.write_read(0x50, &[0x02], &mut read)
            .expect("write_read");
        assert_eq!(read, [0xaa, 0xbb]);
        assert_eq!(
            bus.write(0x51, &[0]),
            Err(VirtualI2cError::NoAcknowledge(NoAcknowledgeSource::Address))
        );
        assert_eq!(
            hardware.read_registers("I2C0", 0x50, 2, 2).expect("peek"),
            [0xaa, 0xbb]
        );
        let events = hardware.events(2);
        assert_eq!(
            events[0].detail,
            EventDetail::I2c {
                bus: "I2C0",
                address: 0x50,
                operations: vec![
                    OperationRecord::Write(String::from("02")),
                    OperationRecord::Read(String::from("aabb")),
                ],
                result: "ok",
                injected: false,
            }
        );
        assert!(matches!(
            events[1].detail,
            EventDetail::I2c { result: "nack", .. }
        ));
    }

    #[test]
    fn adjacent_writes_form_one_phase() {
        let hardware = hardware();
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");
        bus.transaction(
            0x50,
            &mut [Operation::Write(&[0x10]), Operation::Write(&[1, 2])],
        )
        .expect("transaction");
        assert_eq!(
            hardware
                .read_registers("I2C0", 0x50, 0x10, 2)
                .expect("peek"),
            [1, 2]
        );
    }

    #[test]
    fn once_faults_fire_once_and_persistent_faults_until_cleared() {
        let hardware = hardware();
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");
        hardware
            .inject_fault("I2C0", Some(0x50), FaultKind::Timeout, true)
            .expect("fault");
        assert_eq!(bus.write(0x50, &[0]), Err(VirtualI2cError::Timeout));
        assert_eq!(bus.write(0x50, &[0]), Ok(()));
        hardware
            .inject_fault("I2C0", None, FaultKind::ArbitrationLoss, false)
            .expect("fault");
        assert_eq!(bus.write(0x50, &[0]), Err(VirtualI2cError::ArbitrationLoss));
        assert_eq!(bus.write(0x10, &[0]), Err(VirtualI2cError::ArbitrationLoss));
        assert_eq!(hardware.clear_faults(Some("I2C0")).expect("clear"), 1);
        assert_eq!(bus.write(0x50, &[0]), Ok(()));
        assert!(hardware.faults().is_empty());
    }

    #[test]
    fn requests_are_validated() {
        let hardware = hardware();
        assert!(matches!(
            hardware.attach("I2C9", 0x50, Box::new(RegisterDevice::new())),
            Err(HardwareError::UnknownBus(_))
        ));
        assert!(matches!(
            hardware.attach("I2C0", 0x80, Box::new(RegisterDevice::new())),
            Err(HardwareError::InvalidAddress(0x80))
        ));
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        assert!(matches!(
            hardware.attach("I2C0", 0x50, Box::new(RegisterDevice::new())),
            Err(HardwareError::AddressInUse { .. })
        ));
        assert!(matches!(
            hardware.detach("I2C0", 0x51),
            Err(HardwareError::NoDevice { .. })
        ));
        assert!(matches!(
            hardware.pin("nope"),
            Err(HardwareError::UnknownPin(_))
        ));
    }

    #[test]
    fn reset_keeps_system_owned_pin_state() {
        let hardware = hardware();
        hardware.configure_pin(
            0,
            PinMode::Output,
            PinPull::None,
            PinDrive::PushPull,
            Some(true),
        );
        hardware.drive("GPIO1", Some(true)).expect("drive");
        hardware
            .attach("I2C0", 0x50, Box::new(RegisterDevice::new()))
            .expect("attach");
        hardware.reset();
        assert!(hardware.events(0).is_empty());
        assert!(hardware.buses()[0].devices.is_empty());
        assert_eq!(hardware.pin("GPIO1").expect("pin").driven, None);
        assert!(hardware.pin("vio-0").expect("pin").level);
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(unhex("00AB10"), Some(vec![0, 0xab, 0x10]));
        assert_eq!(unhex("0"), None);
        assert_eq!(unhex("zz"), None);
    }
}
