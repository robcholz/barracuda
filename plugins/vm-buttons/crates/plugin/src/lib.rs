//! Lua package for the Board's built-in buttons.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::buttons::{ButtonEvent, Buttons, ButtonsPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Variadic,
};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// The Lua package name.
const PACKAGE: &str = "buttons";

/// How often `wait` polls the buttons.
const WAIT_POLL: Duration = Duration::from_millis(10);

/// Owns the Board's buttons and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct ButtonsPlugin<Device> {
    buttons: Vec<(&'static str, Device)>,
}

impl<Device> ButtonsPlugin<Device> {
    /// Takes the built-in buttons from the generated Board capability set,
    /// the primary device first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: ButtonsPeripheral<Buttons = Device>,
    {
        Self {
            buttons: config::peripherals::BUTTONS_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_buttons(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for ButtonsPlugin<Device>
where
    Device: Buttons + Send + 'static,
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
            .register(ButtonsPackage::new(core::mem::take(&mut self.buttons)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct ButtonsPackage<Device> {
    buttons: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> ButtonsPackage<Device> {
    fn new(buttons: Vec<(&'static str, Device)>) -> Self {
        Self {
            buttons: BoardDevices::new(buttons),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Lends the buttons `name`, or the primary ones, to one call; they go back
/// when the call ends or is cancelled.
fn lend<Device>(
    buttons: &BoardDevices<Device>,
    active: &AtomicBool,
    name: Option<&str>,
) -> Result<LentDevice<Device>> {
    ensure_active(active)?;
    buttons.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("buttons `{name}` are unavailable or busy"),
            None => String::from("built-in buttons are unavailable or busy"),
        })
    })
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("buttons package has been revoked"))
    }
}

fn button_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("buttons read failed: {error:?}"))
}

/// A call's arguments: at most `N`; absent ones are nil.
fn arguments<const N: usize>(values: Variadic<LuaValue>) -> Result<[LuaValue; N]> {
    let values: Vec<LuaValue> = values.into();
    if values.len() > N {
        return Err(Error::runtime(alloc::format!(
            "expected at most {N} arguments"
        )));
    }
    let mut values = values.into_iter();
    Ok(core::array::from_fn(|_| {
        values.next().unwrap_or(LuaValue::Nil)
    }))
}

/// A timeout argument in milliseconds, or `None` to wait without one.
fn timeout(value: &LuaValue) -> Result<Option<Duration>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(milliseconds) => u64::try_from(*milliseconds)
            .map(|milliseconds| Some(Duration::from_millis(milliseconds)))
            .map_err(|_| Error::runtime("a timeout is milliseconds, 0 or more")),
        _ => Err(Error::runtime("expected a timeout in milliseconds")),
    }
}

/// A device name argument, or `None` for the primary buttons when it is nil.
fn device_name(value: LuaValue) -> Result<Option<String>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::String(name) => Ok(Some(name)),
        _ => Err(Error::runtime("expected a buttons device name")),
    }
}

type EventValues = (i64, &'static str);

/// One event as Lua values, the button numbered from 1; nothing when no
/// button changed.
fn event_result(event: Result<Option<ButtonEvent>>) -> Option<Result<EventValues>> {
    match event {
        Ok(Some(event)) => Some(Ok((
            i64::try_from(event.index())
                .unwrap_or(i64::MAX)
                .saturating_add(1),
            event.kind().name(),
        ))),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

/// Polls until an event arrives or `timeout` passes; without a timeout,
/// until an event arrives.
async fn wait<Device>(
    buttons: &BoardDevices<Device>,
    active: &AtomicBool,
    timeout: Option<Duration>,
    name: Option<&str>,
) -> Result<Option<ButtonEvent>>
where
    Device: Buttons,
{
    let deadline = timeout.map(|timeout| Instant::now().saturating_add(timeout));
    let mut lent = lend(buttons, active, name)?;
    loop {
        ensure_active(active)?;
        if let Some(event) = lent.poll_event().map_err(button_error)? {
            return Ok(Some(event));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(None);
        }
        Timer::after(WAIT_POLL).await;
    }
}

impl<Device> Package for ButtonsPackage<Device>
where
    Device: Buttons + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.buttons.clone(), Arc::clone(&self.active));
        let list = self.buttons.clone();
        let count = (self.buttons.clone(), Arc::clone(&self.active));
        let read = (self.buttons.clone(), Arc::clone(&self.active));
        let poll = (self.buttons.clone(), Arc::clone(&self.active));
        let waiting = (self.buttons.clone(), Arc::clone(&self.active));
        lua.register_lib(PACKAGE, move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (buttons, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && buttons.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register("count", move |name: Variadic<String>| {
                let (buttons, active) = &count;
                Some((|| {
                    let mut lent = lend(buttons, active, name.first().map(String::as_str))?;
                    let snapshot = lent.read().map_err(button_error)?;
                    Ok(i64::try_from(snapshot.len()).unwrap_or(i64::MAX))
                })())
            })?;
            package.register_with("read", move |lua, name: Variadic<String>| {
                let (buttons, active) = &read;
                Some((|| -> Result<Table> {
                    let mut lent = lend(buttons, active, name.first().map(String::as_str))?;
                    let snapshot = lent.read().map_err(button_error)?;
                    let states = lua.create_table()?;
                    for (number, index) in (1_i64..).zip(0..snapshot.len()) {
                        states.set(number, snapshot.is_pressed(index).unwrap_or(false))?;
                    }
                    Ok(states)
                })())
            })?;
            package.register("poll", move |name: Variadic<String>| {
                let (buttons, active) = &poll;
                event_result((|| {
                    let mut lent = lend(buttons, active, name.first().map(String::as_str))?;
                    lent.poll_event().map_err(button_error)
                })())
            })?;
            package.register_async("wait", move |values: Variadic<LuaValue>| {
                let (buttons, active) = (waiting.0.clone(), Arc::clone(&waiting.1));
                let parsed = arguments::<2>(values)
                    .and_then(|[limit, name]| Ok((timeout(&limit)?, device_name(name)?)));
                async move {
                    event_result(
                        async {
                            let (limit, name) = parsed?;
                            wait(&buttons, &active, limit, name.as_deref()).await
                        }
                        .await,
                    )
                }
            })
        })
    }
}

impl<Device> LuaPackage for ButtonsPackage<Device>
where
    Device: Buttons + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        PACKAGE
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.buttons.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 1,
            purpose: "Read buttons",
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
    use std::collections::VecDeque;

    use barracuda_board_hal::buttons::{ButtonEventKind, ButtonSnapshot};

    /// Buttons whose states and pending events the test sets.
    struct TestButtons {
        states: Vec<bool>,
        events: VecDeque<ButtonEvent>,
    }

    impl TestButtons {
        fn new(states: &[bool], events: &[(u8, ButtonEventKind)]) -> Self {
            Self {
                states: states.to_vec(),
                events: events
                    .iter()
                    .map(|&(index, kind)| ButtonEvent::new(index, kind))
                    .collect(),
            }
        }
    }

    impl Buttons for TestButtons {
        type Error = Infallible;

        fn read(&mut self) -> core::result::Result<ButtonSnapshot, Self::Error> {
            Ok(ButtonSnapshot::new(&self.states))
        }

        fn poll_event(&mut self) -> core::result::Result<Option<ButtonEvent>, Self::Error> {
            Ok(self.events.pop_front())
        }
    }

    fn run(package: &ButtonsPackage<TestButtons>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_reads_states_and_events_of_the_primary_buttons() {
        let package = ButtonsPackage::new(std::vec![(
            "front",
            TestButtons::new(
                &[false, true, false],
                &[
                    (1, ButtonEventKind::Pressed),
                    (1, ButtonEventKind::Released),
                    (1, ButtonEventKind::Click),
                    (0, ButtonEventKind::LongPress),
                ],
            ),
        )]);
        assert!(run(
            &package,
            "local buttons = require('buttons')\n\
             assert(buttons.available() and buttons.count() == 3)\n\
             local states = buttons.read()\n\
             assert(#states == 3 and states[1] == false and states[2] == true)\n\
             local b1, e1 = buttons.poll()\n\
             local b2, e2 = buttons.poll()\n\
             local b3, e3 = buttons.wait(100)\n\
             local b4, e4 = buttons.wait()\n\
             local none = buttons.poll()\n\
             return b1 == 2 and e1 == 'pressed' and b2 == 2 and e2 == 'released'\n\
               and b3 == 2 and e3 == 'click' and b4 == 1 and e4 == 'long_press'\n\
               and none == nil and buttons.available()",
        ));
    }

    #[test]
    fn wait_returns_nothing_when_the_timeout_passes() {
        let package = ButtonsPackage::new(std::vec![("front", TestButtons::new(&[false], &[]))]);
        assert!(run(
            &package,
            "local buttons = require('buttons')\n\
             local button, event = buttons.wait(30)\n\
             local ok, err = buttons.wait(-1)\n\
             return button == nil and event == nil and ok == nil and err:find('timeout') ~= nil",
        ));
    }

    #[test]
    fn lua_names_a_device_and_defaults_to_the_primary() {
        let package = ButtonsPackage::new(std::vec![
            ("front", TestButtons::new(&[true], &[])),
            (
                "side",
                TestButtons::new(&[false, false], &[(0, ButtonEventKind::Pressed)]),
            ),
        ]);
        assert!(run(
            &package,
            "local buttons = require('buttons')\n\
             local names = buttons.list()\n\
             assert(#names == 2 and names[1] == 'front' and names[2] == 'side')\n\
             assert(buttons.count() == 1 and buttons.count('side') == 2)\n\
             assert(buttons.read()[1] == true and buttons.read('side')[1] == false)\n\
             assert(buttons.poll() == nil)\n\
             local button, event = buttons.wait(nil, 'side')\n\
             local ok, err = buttons.read('missing')\n\
             return button == 1 and event == 'pressed' and ok == nil and err:find('missing') ~= nil\n\
               and not buttons.available('missing')",
        ));
    }

    #[test]
    fn a_board_without_buttons_reports_unavailable() {
        let package = ButtonsPackage::<TestButtons>::new(Vec::new());
        assert!(package.interface().is_none());
        assert!(run(
            &package,
            "local buttons = require('buttons')\n\
             local states, err = buttons.read()\n\
             return not buttons.available() and #buttons.list() == 0 and states == nil\n\
               and err:find('unavailable') ~= nil",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = ButtonsPackage::new(std::vec![("front", TestButtons::new(&[false], &[]))]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
