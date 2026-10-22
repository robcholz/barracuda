//! Lua package for the Board's primary built-in NFC card reader.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::nfc_reader::{Card, NfcReader, NfcReaderPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package, Result, Table, Variadic,
};
use core::{cell::RefCell, fmt::Write as _};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// How often `wait` looks for a card.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// Owns the Board's primary NFC card reader and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct NfcPlugin<Device> {
    reader: Option<Device>,
}

impl<Device> NfcPlugin<Device> {
    /// Takes the built-in card reader from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: NfcReaderPeripheral<NfcReader = Device>,
    {
        Self {
            reader: config::peripherals::NFC_READER_DEVICE
                .and_then(|instance| context.hal.peripherals.take_nfc_reader(instance)),
        }
    }
}

impl<Device> Plugin for NfcPlugin<Device>
where
    Device: NfcReader + Send + 'static,
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
            .register(NfcPackage::new(self.reader.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The reader while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The reader taken out of its slot for one call; it goes back when the call
/// ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("nfc package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime("built-in NFC reader is unavailable or busy")),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in NFC reader is unavailable or busy"))
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

struct NfcPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    reader: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> NfcPackage<Device> {
    fn new(reader: Option<Device>) -> Self {
        Self {
            present: reader.is_some(),
            reader: Arc::new(critical_section::Mutex::new(RefCell::new(reader))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn reader_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("NFC operation failed: {error:?}"))
}

/// Lowercase hexadecimal, two digits a byte.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Fills `table` with `{uid = "04a1b2c3", atqa = 68, sak = 0}`.
fn describe(table: &Table, card: &Card) -> Result<()> {
    table.set("uid", hex(card.uid().as_bytes()))?;
    table.set("atqa", i64::from(card.atqa()))?;
    table.set("sak", i64::from(card.sak()))
}

/// An optional timeout in milliseconds as the only argument.
fn timeout(values: Variadic<LuaValue>) -> Result<Option<Duration>> {
    let values: Vec<LuaValue> = values.into();
    match values.as_slice() {
        [] | [LuaValue::Nil] => Ok(None),
        [LuaValue::Integer(milliseconds)] => u64::try_from(*milliseconds)
            .map(|milliseconds| Some(Duration::from_millis(milliseconds)))
            .map_err(|_| Error::runtime("a timeout is milliseconds, 0 or more")),
        _ => Err(Error::runtime(
            "expected an optional timeout in milliseconds",
        )),
    }
}

type CardResult = (Option<Table>, Option<String>);

impl<Device> Package for NfcPackage<Device>
where
    Device: NfcReader + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (Arc::clone(&self.reader), Arc::clone(&self.active));
        let poll = (Arc::clone(&self.reader), Arc::clone(&self.active));
        let wait = (Arc::clone(&self.reader), Arc::clone(&self.active));
        let read = (Arc::clone(&self.reader), Arc::clone(&self.active));
        lua.register_lib("nfc", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_async_with("poll", move |lua, (): ()| {
                let (slot, active) = (Arc::clone(&poll.0), Arc::clone(&poll.1));
                let table = lua.create_table();
                async move {
                    Some(
                        async {
                            let table = table?;
                            let mut lent = Lent::take(&slot, &active)?;
                            match lent.device()?.poll().await.map_err(reader_error)? {
                                Some(card) => {
                                    describe(&table, &card)?;
                                    Ok(Some(table))
                                }
                                None => Ok(None),
                            }
                        }
                        .await,
                    ) as Option<Result<Option<Table>>>
                }
            })?;
            package.register_async_with("wait", move |lua, values: Variadic<LuaValue>| {
                let (slot, active) = (Arc::clone(&wait.0), Arc::clone(&wait.1));
                let table = lua.create_table();
                let limit = timeout(values);
                async move {
                    Some(
                        async {
                            let table = table?;
                            let limit = limit?;
                            let mut lent = Lent::take(&slot, &active)?;
                            let deadline =
                                limit.and_then(|limit| Instant::now().checked_add(limit));
                            loop {
                                if let Some(card) =
                                    lent.device()?.poll().await.map_err(reader_error)?
                                {
                                    describe(&table, &card)?;
                                    return Ok((Some(table), None));
                                }
                                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                                    return Ok((None, Some(String::from("timeout"))));
                                }
                                Timer::after(WAIT_POLL).await;
                            }
                        }
                        .await,
                    ) as Option<Result<CardResult>>
                }
            })?;
            package.register_async("read", move |page: i64| {
                let (slot, active) = (Arc::clone(&read.0), Arc::clone(&read.1));
                async move {
                    Some(
                        async {
                            let page = u8::try_from(page)
                                .map_err(|_| Error::runtime("a page is 0 through 255"))?;
                            let mut lent = Lent::take(&slot, &active)?;
                            let block = lent
                                .device()?
                                .read_block(page)
                                .await
                                .map_err(reader_error)?;
                            Bytes::copy_from(&block)
                        }
                        .await,
                    )
                }
            })
        })
    }
}

impl<Device> LuaPackage for NfcPackage<Device>
where
    Device: NfcReader + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "nfc"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
                purpose: "读取 NFC 卡片",
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
    use barracuda_board_hal::nfc_reader::{BLOCK_BYTES, Uid};

    /// A reader that finds a card on its second poll.
    #[derive(Default)]
    struct TestReader {
        polls: u32,
    }

    impl NfcReader for TestReader {
        type Error = &'static str;

        async fn poll(&mut self) -> core::result::Result<Option<Card>, Self::Error> {
            self.polls += 1;
            Ok((self.polls >= 2).then(|| {
                Card::new(
                    Uid::new(&[0x04, 0xa1, 0xb2, 0xc3]).expect("UID"),
                    0x0044,
                    0x00,
                )
            }))
        }

        async fn read_block(
            &mut self,
            block: u8,
        ) -> core::result::Result<[u8; BLOCK_BYTES], Self::Error> {
            if self.polls >= 2 {
                Ok([block; BLOCK_BYTES])
            } else {
                Err("no card")
            }
        }
    }

    fn run(package: &NfcPackage<TestReader>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_polls_waits_and_reads_a_card() {
        let package = NfcPackage::new(Some(TestReader::default()));
        assert!(run(
            &package,
            "local nfc = require('nfc')\n\
             assert(nfc.available())\n\
             assert(nfc.poll() == nil)\n\
             local card = nfc.wait(1000)\n\
             assert(card.uid == '04a1b2c3' and card.atqa == 68 and card.sak == 0)\n\
             local data = nfc.read(4)\n\
             return #data == 16 and data:byte(1) == 4",
        ));
    }

    #[test]
    fn a_board_without_reader_reports_none() {
        let package = NfcPackage::<TestReader>::new(None);
        assert!(run(
            &package,
            "local nfc = require('nfc')\n\
             local card, err = nfc.poll()\n\
             return not nfc.available() and card == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = NfcPackage::new(Some(TestReader::default()));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
