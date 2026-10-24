//! Lua package for the Board's built-in keyboards.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{format, string::String, vec::Vec};
use barracuda_board_hal::keyboard::{Key, KeyEvent, Keyboard, KeyboardPeripheral, Modifiers};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result,
    UserData, UserDataHandle, UserDataMethods, Variadic,
};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// How often `wait` polls the keyboard.
const WAIT_POLL: Duration = Duration::from_millis(10);

/// Owns the Board's keyboards and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct KeyboardPlugin<Device> {
    keyboards: Vec<(&'static str, Device)>,
}

impl<Device> KeyboardPlugin<Device> {
    /// Takes the built-in keyboards from the generated Board capability
    /// set, the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: KeyboardPeripheral<Keyboard = Device>,
    {
        Self {
            keyboards: config::peripherals::KEYBOARD_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_keyboard(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for KeyboardPlugin<Device>
where
    Device: Keyboard + Send + 'static,
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
            .register(KeyboardPackage::new(core::mem::take(&mut self.keyboards)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct KeyboardPackage<Device> {
    keyboards: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> KeyboardPackage<Device> {
    fn new(keyboards: Vec<(&'static str, Device)>) -> Self {
        Self {
            keyboards: BoardDevices::new(keyboards),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for KeyboardPackage<Device>
where
    Device: Keyboard + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.keyboards.clone(), Arc::clone(&self.active));
        let list = self.keyboards.clone();
        let open = (self.keyboards.clone(), Arc::clone(&self.active));
        lua.register_lib("keyboard", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (devices, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && devices.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_with("open", move |lua, name: Variadic<String>| {
                let (devices, active) = &open;
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("keyboard package has been revoked")));
                }
                let name = name.first().map(String::as_str);
                Some(match devices.take(name) {
                    Some(device) => {
                        lua.create_userdata(KeyboardHandle::new(device, Arc::clone(active)))
                    }
                    None => Err(Error::runtime(match name {
                        Some(name) => {
                            alloc::format!("keyboard `{name}` is unavailable or already open")
                        }
                        None => String::from("built-in keyboard is unavailable or already open"),
                    })),
                })
            })
        })
    }
}

impl<Device> LuaPackage for KeyboardPackage<Device>
where
    Device: Keyboard + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "keyboard"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.keyboards.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "Read the keyboard",
        })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct KeyboardHandle<Device> {
    keyboard: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> KeyboardHandle<Device> {
    fn new(keyboard: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            keyboard: Some(keyboard),
            active,
        }
    }

    fn close(&mut self) {
        self.keyboard.take();
    }
}

impl<Device> KeyboardHandle<Device>
where
    Device: Keyboard,
    Device::Error: core::fmt::Debug,
{
    fn poll(&mut self) -> Result<Option<KeyEvent>> {
        ensure_active(&self.active)?;
        self.keyboard
            .as_mut()
            .ok_or_else(|| Error::runtime("keyboard handle is closed"))?
            .poll_event()
            .map_err(|error| Error::runtime(format!("keyboard read failed: {error:?}")))
    }
}

impl<Device> UserData for KeyboardHandle<Device>
where
    Device: Keyboard + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method_mut("read", |handle, (): ()| event_result(handle.poll()));
        methods.add_async_method("wait", |handle, timeout_ms: i64| async move {
            event_result(wait(handle, timeout_ms).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.keyboard.is_some()
            ))
        });
        methods.add_method_mut("close", |handle, (): ()| {
            handle.close();
            None::<Result<()>>
        });
        methods.add_meta_method_mut(MetaMethod::Close, |handle, _error: Option<String>| {
            handle.close();
            None::<Result<()>>
        });
    }
}

impl<Device> Drop for KeyboardHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Polls until an event arrives or `timeout_ms` passes.
async fn wait<Device>(
    handle: UserDataHandle<KeyboardHandle<Device>>,
    timeout_ms: i64,
) -> Result<Option<KeyEvent>>
where
    Device: Keyboard + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
    let deadline = Instant::now().saturating_add(timeout);
    loop {
        let event = handle.borrow_mut()?.poll()?;
        if event.is_some() || Instant::now() >= deadline {
            return Ok(event);
        }
        Timer::after(WAIT_POLL).await;
    }
}

type EventValues = (String, bool, Option<String>, i64, i64);

/// One event as Lua values; nothing when no key changed.
fn event_result(event: Result<Option<KeyEvent>>) -> Option<Result<EventValues>> {
    match event {
        Ok(Some(event)) => Some(Ok(event_values(event))),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

fn event_values(event: KeyEvent) -> EventValues {
    (
        key_name(event.key()),
        event.is_pressed(),
        event.text().map(String::from),
        i64::from(modifier_bits(event.modifiers())),
        i64::from(event.code()),
    )
}

fn key_name(key: Key) -> String {
    match key {
        Key::Character(character) => String::from(character),
        Key::Enter => String::from("enter"),
        Key::Backspace => String::from("backspace"),
        Key::Delete => String::from("delete"),
        Key::Tab => String::from("tab"),
        Key::Escape => String::from("escape"),
        Key::Shift => String::from("shift"),
        Key::Control => String::from("control"),
        Key::Alt => String::from("alt"),
        Key::Option => String::from("option"),
        Key::Function => String::from("fn"),
        Key::Up => String::from("up"),
        Key::Down => String::from("down"),
        Key::Left => String::from("left"),
        Key::Right => String::from("right"),
        Key::F(number) => format!("f{number}"),
        _ => String::from("unknown"),
    }
}

/// Shift 1, Control 2, Alt 4, Option 8, Fn 16.
fn modifier_bits(modifiers: Modifiers) -> u8 {
    [
        (Modifiers::SHIFT, 1),
        (Modifiers::CONTROL, 2),
        (Modifiers::ALT, 4),
        (Modifiers::OPTION, 8),
        (Modifiers::FUNCTION, 16),
    ]
    .into_iter()
    .filter(|(modifier, _)| modifiers.contains(*modifier))
    .fold(0, |bits, (_, bit)| bits | bit)
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("keyboard package has been revoked"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;
    use std::collections::VecDeque;

    use barracuda_board_hal::keyboard::KeyState;

    struct TestKeyboard(VecDeque<KeyEvent>);

    impl Keyboard for TestKeyboard {
        type Error = Infallible;

        fn poll_event(&mut self) -> core::result::Result<Option<KeyEvent>, Self::Error> {
            Ok(self.0.pop_front())
        }
    }

    fn keyboard() -> TestKeyboard {
        TestKeyboard(VecDeque::from([
            KeyEvent::new(Key::Shift, KeyState::Pressed, Modifiers::SHIFT, None, 29),
            KeyEvent::new(
                Key::Character('a'),
                KeyState::Pressed,
                Modifiers::SHIFT,
                Some('A'),
                30,
            ),
            KeyEvent::new(Key::F(5), KeyState::Released, Modifiers::empty(), None, 5),
        ]))
    }

    #[test]
    fn lua_reads_key_events() {
        let package = KeyboardPackage::new(std::vec![("keys", keyboard())]);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local keyboard = require('keyboard')\nlocal device <close> = keyboard.open()\nlocal k1, p1, t1, m1 = device:read()\nlocal k2, p2, t2, m2, c2 = device:read()\nlocal k3, p3 = device:read()\nlocal k4 = device:read()\nreturn keyboard.available() == false and k1 == 'shift' and p1 and t1 == nil and m1 == 1 and k2 == 'a' and t2 == 'A' and c2 == 30 and k3 == 'f5' and p3 == false and k4 == nil")
                .eval_async(),
        )
        .expect("read keyboard");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = KeyboardPackage::new(std::vec![("keys", keyboard())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }

    #[test]
    fn lua_lists_and_opens_each_device_the_primary_by_default() {
        let package = KeyboardPackage::new(std::vec![("keys", keyboard()), ("side", keyboard())]);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local devices = require('keyboard')\nlocal names = devices.list()\nlocal second <close> = devices.open(names[2])\nlocal first <close> = devices.open()\nlocal again, problem = devices.open(names[2])\nreturn #names == 2 and names[1] == 'keys' and first:is_open() and second:is_open() and again == nil and problem ~= nil and not devices.available() and not devices.available('missing')")
                .eval_async(),
        )
        .expect("open devices");
        assert!(result);
    }
}
