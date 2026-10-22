//! Lua package for the Board's built-in buzzers.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::buzzer::{Buzzer, BuzzerPeripheral, Note, Volume};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Volume of a tone whose script names none, in percent.
const DEFAULT_VOLUME: u8 = 50;
/// Most notes one melody may hold.
const MAX_NOTES: usize = 256;

/// Owns the Board's buzzers and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct BuzzerPlugin<Device> {
    buzzers: Vec<(&'static str, Device)>,
}

impl<Device> BuzzerPlugin<Device> {
    /// Takes the built-in buzzers from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: BuzzerPeripheral<Buzzer = Device>,
    {
        Self {
            buzzers: config::peripherals::BUZZER_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_buzzer(*instance)
                        .map(|buzzer| (*name, buzzer))
                })
                .collect(),
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
            .register(BuzzerPackage::new(core::mem::take(&mut self.buzzers)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Lends the buzzer `name`, or the primary one, to one call; it goes back
/// when the call ends or is cancelled.
fn lend<Device>(
    buzzers: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
) -> Result<LentDevice<Device>> {
    if !active.load(Ordering::Acquire) {
        return Err(Error::runtime("buzzer package has been revoked"));
    }
    buzzers.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("buzzer `{name}` is unavailable or busy"),
            None => String::from("built-in buzzer is unavailable or busy"),
        })
    })
}

struct BuzzerPackage<Device> {
    buzzers: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> BuzzerPackage<Device> {
    fn new(buzzers: Vec<(&'static str, Device)>) -> Self {
        Self {
            buzzers: BoardDevices::new(buzzers),
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

/// A device name argument, or `None` for the primary device when it is nil.
fn device_name(value: LuaValue) -> Result<Option<String>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(name) => Ok(Some(name)),
        _ => Err(Error::runtime("expected a buzzer name")),
    }
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
        let available = (self.buzzers.clone(), Arc::clone(&self.active));
        let list = self.buzzers.clone();
        let range = (self.buzzers.clone(), Arc::clone(&self.active));
        let start = (self.buzzers.clone(), Arc::clone(&self.active));
        let stop = (self.buzzers.clone(), Arc::clone(&self.active));
        let tone = (self.buzzers.clone(), Arc::clone(&self.active));
        let play = (self.buzzers.clone(), Arc::clone(&self.active));
        lua.register_lib("buzzer", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (buzzers, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && buzzers.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("range", move |name: Variadic<String>| {
                let (buzzers, active) = &range;
                Some(
                    lend(buzzers, active, name.first().map(String::as_str))
                        .map(|lent| lent.frequency_range())
                        .map(|(lowest, highest)| (i64::from(lowest), i64::from(highest))),
                )
            })?;
            package.register("start", move |values: Variadic<LuaValue>| {
                let (buzzers, active) = &start;
                Some((|| {
                    let [frequency_hz, percent, name] = arguments(values, 1)?;
                    let frequency_hz = integer(&frequency_hz)?;
                    let volume = volume(optional_integer(&percent)?)?;
                    let frequency_hz = u32::try_from(frequency_hz)
                        .map_err(|_| Error::runtime("a frequency is positive hertz"))?;
                    let name = device_name(name)?;
                    let mut lent = lend(buzzers, active, name.as_deref())?;
                    lent.start(frequency_hz, volume).map_err(buzzer_error)?;
                    Ok(true)
                })())
            })?;
            package.register("stop", move |name: Variadic<String>| {
                let (buzzers, active) = &stop;
                Some((|| {
                    let mut lent = lend(buzzers, active, name.first().map(String::as_str))?;
                    lent.stop().map_err(buzzer_error)?;
                    Ok(true)
                })())
            })?;
            package.register_async("tone", move |values: Variadic<LuaValue>| {
                let (buzzers, active) = (tone.0.clone(), Arc::clone(&tone.1));
                let parsed =
                    arguments(values, 2).and_then(|[frequency, duration, percent, name]| {
                        Ok((
                            note(integer(&frequency)?, integer(&duration)?)?,
                            volume(optional_integer(&percent)?)?,
                            device_name(name)?,
                        ))
                    });
                async move {
                    Some(
                        async {
                            let (note, volume, name) = parsed?;
                            let mut lent = lend(&buzzers, &active, name.as_deref())?;
                            lent.play_note(note, volume).await.map_err(buzzer_error)?;
                            Ok(true)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("play", move |values: Variadic<LuaValue>| {
                let (buzzers, active) = (play.0.clone(), Arc::clone(&play.1));
                let parsed = arguments(values, 1).and_then(|[notes, percent, name]| {
                    Ok((
                        melody(&table(notes)?)?,
                        volume(optional_integer(&percent)?)?,
                        device_name(name)?,
                    ))
                });
                async move {
                    Some(
                        async {
                            let (melody, volume, name) = parsed?;
                            let mut lent = lend(&buzzers, &active, name.as_deref())?;
                            lent.play(&melody, volume).await.map_err(buzzer_error)?;
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
        (!self.buzzers.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
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
    use core::cell::RefCell;
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
        let package = BuzzerPackage::new(std::vec![("main", buzzer)]);
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
    fn lua_names_a_buzzer_and_defaults_to_the_primary() {
        let main = TestBuzzer::default();
        let side = TestBuzzer::default();
        let (main_events, side_events) = (Arc::clone(&main.events), Arc::clone(&side.events));
        let package = BuzzerPackage::new(std::vec![("main", main), ("side", side)]);
        assert!(run(
            &package,
            "local buzzer = require('buzzer')\n\
             local names = buzzer.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(buzzer.available('side') and not buzzer.available('missing'))\n\
             assert(buzzer.start(1000))\n\
             assert(buzzer.start(2000, 30, 'side'))\n\
             assert(buzzer.stop('side'))\n\
             local ok, err = buzzer.tone(440, 10, nil, 'missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
        let events = |log: &Log<(u32, u8)>| {
            critical_section::with(|section| log.borrow(section).borrow().clone())
        };
        assert_eq!(events(&main_events), [(1000, 50)]);
        assert_eq!(events(&side_events), [(2000, 30), (0, 0)]);
    }

    #[test]
    fn a_board_without_buzzer_reports_none() {
        let package = BuzzerPackage::<TestBuzzer>::new(Vec::new());
        assert!(run(
            &package,
            "local buzzer = require('buzzer')\n\
             local ok, err = buzzer.tone(440, 10)\n\
             return not buzzer.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = BuzzerPackage::new(std::vec![("main", TestBuzzer::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
