//! Lua package for the Board's built-in NFC readers.

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
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Trailing, Variadic,
};
use core::fmt::Write as _;
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// How often `wait` looks for a card.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// Owns the Board's NFC readers and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct NfcPlugin<Device> {
    readers: Vec<(&'static str, Device)>,
}

impl<Device> NfcPlugin<Device> {
    /// Takes the built-in NFC readers from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: NfcReaderPeripheral<NfcReader = Device>,
    {
        Self {
            readers: config::peripherals::NFC_READER_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_nfc_reader(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
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
            .register(NfcPackage::new(core::mem::take(&mut self.readers)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The NFC reader lent to one call; it goes back when the call ends or is
/// cancelled.
struct Lent<Device>(LentDevice<Device>);

impl<Device> Lent<Device> {
    /// Lends the NFC reader `name`, or the primary one.
    fn take(
        devices: &BoardDevices<Device>,
        active: &AtomicBool,
        name: Option<&str>,
    ) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("nfc package has been revoked"));
        }
        devices.lend(name).map(Self).ok_or_else(|| {
            Error::runtime(match name {
                Some(name) => alloc::format!("NFC reader `{name}` is unavailable or busy"),
                None => String::from("built-in NFC reader is unavailable or busy"),
            })
        })
    }

    fn device(&mut self) -> &mut Device {
        &mut self.0
    }
}

struct NfcPackage<Device> {
    readers: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> NfcPackage<Device> {
    fn new(readers: Vec<(&'static str, Device)>) -> Self {
        Self {
            readers: BoardDevices::new(readers),
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
fn timeout(values: Variadic<LuaValue>) -> Result<(Option<Duration>, Option<String>)> {
    let values: Vec<LuaValue> = values.into();
    let (limit, name) = match values.as_slice() {
        [] => (&LuaValue::Nil, &LuaValue::Nil),
        [limit] => (limit, &LuaValue::Nil),
        [limit, name] => (limit, name),
        _ => {
            return Err(Error::runtime(
                "expected an optional timeout in milliseconds and reader name",
            ));
        }
    };
    let limit = match limit {
        LuaValue::Nil => None,
        LuaValue::Integer(milliseconds) => Some(Duration::from_millis(
            u64::try_from(*milliseconds)
                .map_err(|_| Error::runtime("a timeout is milliseconds, 0 or more"))?,
        )),
        _ => return Err(Error::runtime("a timeout is milliseconds, 0 or more")),
    };
    let name = match name {
        LuaValue::Nil => None,
        LuaValue::String(name) => Some(name.clone()),
        _ => return Err(Error::runtime("expected an NFC reader name")),
    };
    Ok((limit, name))
}

type CardResult = (Option<Table>, Option<String>);

impl<Device> Package for NfcPackage<Device>
where
    Device: NfcReader + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.readers.clone(), Arc::clone(&self.active));
        let list = self.readers.clone();
        let poll = (self.readers.clone(), Arc::clone(&self.active));
        let wait = (self.readers.clone(), Arc::clone(&self.active));
        let read = (self.readers.clone(), Arc::clone(&self.active));
        lua.register_lib("nfc", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (readers, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && readers.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_async_with("poll", move |lua, name: Variadic<String>| {
                let name = name.first().cloned();
                let (slot, active) = (poll.0.clone(), Arc::clone(&poll.1));
                let table = lua.create_table();
                async move {
                    Some(
                        async {
                            let table = table?;
                            let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                            match lent.device().poll().await.map_err(reader_error)? {
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
                let (slot, active) = (wait.0.clone(), Arc::clone(&wait.1));
                let table = lua.create_table();
                let limit = timeout(values);
                async move {
                    Some(
                        async {
                            let table = table?;
                            let (limit, name) = limit?;
                            let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                            let deadline =
                                limit.and_then(|limit| Instant::now().checked_add(limit));
                            loop {
                                if let Some(card) =
                                    lent.device().poll().await.map_err(reader_error)?
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
            package.register_async(
                "read",
                move |Trailing((page,), name): Trailing<(i64,), String>| {
                    let (slot, active) = (read.0.clone(), Arc::clone(&read.1));
                    async move {
                        Some(
                            async {
                                let page = u8::try_from(page)
                                    .map_err(|_| Error::runtime("a page is 0 through 255"))?;
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                let block =
                                    lent.device().read_block(page).await.map_err(reader_error)?;
                                Bytes::copy_from(&block)
                            }
                            .await,
                        )
                    }
                },
            )
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
        (!self.readers.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "Read NFC cards",
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
        let package = NfcPackage::new(std::vec![("main", TestReader::default())]);
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
    fn lua_names_a_reader_and_defaults_to_the_primary() {
        let package = NfcPackage::new(std::vec![
            ("main", TestReader::default()),
            ("side", TestReader::default()),
        ]);
        assert!(run(
            &package,
            "local nfc = require('nfc')\n\
             local names = nfc.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(nfc.poll('side') == nil)\n\
             assert(nfc.wait(1000, 'side').uid == '04a1b2c3')\n\
             assert(#nfc.read(1, 'side') == 16)\n\
             local data, err = nfc.read(1)\n\
             assert(data == nil and err:find('no card'))\n\
             data, err = nfc.read(1, 'missing')\n\
             return data == nil and err:find('missing') ~= nil",
        ));
    }

    #[test]
    fn a_board_without_reader_reports_none() {
        let package = NfcPackage::<TestReader>::new(Vec::new());
        assert!(run(
            &package,
            "local nfc = require('nfc')\n\
             local card, err = nfc.poll()\n\
             return not nfc.available() and card == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = NfcPackage::new(std::vec![("main", TestReader::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
