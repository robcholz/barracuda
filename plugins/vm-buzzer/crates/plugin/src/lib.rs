//! Lua package for the Board's primary built-in buzzer.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::vec::Vec;
use barracuda_board_hal::buzzer::{Buzzer, BuzzerPeripheral, Note, Volume};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result, Table, Variadic,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Volume of a tone whose script names none, in percent.
const DEFAULT_VOLUME: u8 = 50;
/// Most notes one melody may hold.
const MAX_NOTES: usize = 256;

/// Owns the Board's primary buzzer and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct BuzzerPlugin<Device> {
    buzzer: Option<Device>,
}

impl<Device> BuzzerPlugin<Device> {
    /// Takes the built-in buzzer from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: BuzzerPeripheral<Buzzer = Device>,
    {
        Self {
            buzzer: config::peripherals::BUZZER_DEVICE
                .and_then(|instance| context.hal.peripherals.take_buzzer(instance)),
        }
    }
}

impl<Device> Plugin for BuzzerPlugin<Device>
where
    Device: Buzzer + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(BuzzerPackage::new(self.buzzer.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The buzzer while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The buzzer taken out of its slot for one call; it goes back when the call
/// ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("buzzer package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime("built-in buzzer is unavailable or busy")),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in buzzer is unavailable or busy"))
    }
}

impl<Device> Drop for Lent<Device> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            critical_section::with(|section| {
                self.home.borrow(section).replace(Some(device));
            });
        }
    }
}

struct BuzzerPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    buzzer: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> BuzzerPackage<Device> {
    fn new(buzzer: Option<Device>) -> Self {
        Self {
            present: buzzer.is_some(),
            buzzer: Arc::new(critical_section::Mutex::new(RefCell::new(buzzer))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn volume(percent: Option<i64>) -> Result<Volume> {
    match percent {
        None => Ok(Volume::percent(DEFAULT_VOLUME)),
        Some(percent) => u8::try_from(percent)
            .ok()
            .filter(|percent| *percent <= 100)
            .map(Volume::percent)
            .ok_or_else(|| Error::runtime("a volume is 0 through 100")),
    }
}

fn note(frequency_hz: i64, duration_ms: i64) -> Result<Note> {
    match (u32::try_from(frequency_hz), u32::try_from(duration_ms)) {
        (Ok(frequency_hz), Ok(duration_ms)) => Ok(Note::new(frequency_hz, duration_ms)),
        _ => Err(Error::runtime(
            "a note is a frequency in hertz (0 rests) and a duration in milliseconds",
        )),
    }
}

/// Reads `{ {frequency, duration}, ... }`.
fn melody(notes: &Table) -> Result<Vec<Note>> {
    let length = notes.len()?;
    if length > MAX_NOTES {
        return Err(Error::runtime("a melody holds at most 256 notes"));
    }
    let mut melody = Vec::with_capacity(length);
    for index in 1..=length {
        let entry: Table = notes.get(i64::try_from(index).unwrap_or(i64::MAX))?;
        melody.push(note(entry.get(1i64)?, entry.get(2i64)?)?);
    }
    Ok(melody)
}

/// A call's arguments: at least `required`, at most `N`; absent ones are nil.
fn arguments<const N: usize>(values: Variadic<LuaValue>, required: usize) -> Result<[LuaValue; N]> {
    let values: Vec<LuaValue> = values.into();
    if values.len() < required || values.len() > N {
        return Err(Error::runtime(alloc::format!(
            "expected {required} to {N} arguments"
        )));
    }
    let mut values = values.into_iter();
    Ok(core::array::from_fn(|_| {
        values.next().unwrap_or(LuaValue::Nil)
    }))
}

/// An integer argument, or `None` when it is nil.
fn optional_integer(value: &LuaValue) -> Result<Option<i64>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(value) => Ok(Some(*value)),
        _ => Err(Error::runtime("expected an integer argument")),
    }
}

/// An integer argument that must be given.
fn integer(value: &LuaValue) -> Result<i64> {
    optional_integer(value)?.ok_or_else(|| Error::runtime("expected an integer argument"))
}

/// A table argument.
fn table(value: LuaValue) -> Result<Table> {
    match value {
        LuaValue::Table(table) => Ok(table),
        _ => Err(Error::runtime("expected a table argument")),
    }
}

fn buzzer_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("buzzer operation failed: {error:?}"))
}

impl<Device> Package for BuzzerPackage<Device>
where
    Device: Buzzer + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        let range = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        let start = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        let stop = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        let tone = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        let play = (Arc::clone(&self.buzzer), Arc::clone(&self.active));
        lua.register_lib("buzzer", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register("range", move |(): ()| {
                let (slot, active) = &range;
                Some(
                    Lent::take(slot, active)
                        .and_then(|mut lent| Ok(lent.device()?.frequency_range()))
                        .map(|(lowest, highest)| (i64::from(lowest), i64::from(highest))),
                )
            })?;
            package.register("start", move |values: Variadic<LuaValue>| {
                let (slot, active) = &start;
                Some((|| {
                    let [frequency_hz, percent] = arguments(values, 1)?;
                    let frequency_hz = integer(&frequency_hz)?;
                    let volume = volume(optional_integer(&percent)?)?;
                    let frequency_hz = u32::try_from(frequency_hz)
                        .map_err(|_| Error::runtime("a frequency is positive hertz"))?;
                    let mut lent = Lent::take(slot, active)?;
                    lent.device()?
                        .start(frequency_hz, volume)
                        .map_err(buzzer_error)?;
                    Ok(true)
                })())
            })?;
            package.register("stop", move |(): ()| {
                let (slot, active) = &stop;
                Some((|| {
                    let mut lent = Lent::take(slot, active)?;
                    lent.device()?.stop().map_err(buzzer_error)?;
                    Ok(true)
                })())
            })?;
            package.register_async("tone", move |values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&tone.0), Arc::clone(&tone.1));
                let parsed = arguments(values, 2).and_then(|[frequency, duration, percent]| {
                    Ok((
                        note(integer(&frequency)?, integer(&duration)?)?,
                        volume(optional_integer(&percent)?)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (note, volume) = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .play_note(note, volume)
                                .await
                                .map_err(buzzer_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("play", move |values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&play.0), Arc::clone(&play.1));
                let parsed = arguments(values, 1).and_then(|[notes, percent]| {
                    Ok((
                        melody(&table(notes)?)?,
                        volume(optional_integer(&percent)?)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (melody, volume) = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .play(&melody, volume)
                                .await
                                .map_err(buzzer_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })
        })
    }
}

impl<Device> LuaPackage for BuzzerPackage<Device>
where
    Device: Buzzer + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "buzzer"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "播放提示音",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;
    use std::vec::Vec;

    /// What a test device records, shared with the test.
    type Log<T> = Arc<critical_section::Mutex<RefCell<Vec<T>>>>;

    /// Records each tone and stop.
    struct TestBuzzer {
        events: Log<(u32, u8)>,
    }

    impl Default for TestBuzzer {
        fn default() -> Self {
            Self {
                events: Arc::new(critical_section::Mutex::new(RefCell::new(Vec::new()))),
            }
        }
    }

    impl Buzzer for TestBuzzer {
        type Error = Infallible;

        fn frequency_range(&self) -> (u32, u32) {
            (76, 78_125)
        }

        fn start(
            &mut self,
            frequency_hz: u32,
            volume: Volume,
        ) -> core::result::Result<(), Infallible> {
            critical_section::with(|section| {
                self.events
                    .borrow(section)
                    .borrow_mut()
                    .push((frequency_hz, volume.get()));
            });
            Ok(())
        }

        fn stop(&mut self) -> core::result::Result<(), Infallible> {
            self.start(0, Volume::percent(0))
        }

        async fn play_note(
            &mut self,
            note: Note,
            volume: Volume,
        ) -> core::result::Result<(), Infallible> {
            self.start(note.frequency_hz(), volume)?;
            self.stop()
        }
    }

    fn run(package: &BuzzerPackage<TestBuzzer>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_plays_tones_and_melodies() {
        let buzzer = TestBuzzer::default();
        let events = Arc::clone(&buzzer.events);
        let package = BuzzerPackage::new(Some(buzzer));
        assert!(run(
            &package,
            "local buzzer = require('buzzer')\n\
             assert(buzzer.available())\n\
             local low, high = buzzer.range()\n\
             assert(low == 76 and high == 78125)\n\
             assert(buzzer.tone(440, 100, 80))\n\
             assert(buzzer.play({ {523, 50}, {0, 50} }))\n\
             assert(buzzer.start(1000))\n\
             assert(buzzer.stop())\n\
             local ok, err = buzzer.tone(440, 100, 150)\n\
             return ok == nil and err:find('volume') ~= nil",
        ));
        let events = critical_section::with(|section| events.borrow(section).borrow().clone());
        assert_eq!(
            events,
            [
                (440, 80),
                (0, 0),
                (523, 50),
                (0, 0),
                (0, 50),
                (0, 0),
                (1000, 50),
                (0, 0)
            ]
        );
    }

    #[test]
    fn a_board_without_buzzer_reports_none() {
        let package = BuzzerPackage::<TestBuzzer>::new(None);
        assert!(run(
            &package,
            "local buzzer = require('buzzer')\n\
             local ok, err = buzzer.tone(440, 10)\n\
             return not buzzer.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = BuzzerPackage::new(Some(TestBuzzer::default()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
