//! Lua package for the Board's built-in secure elements.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::secure_element::{KeySlot, SecureElement, SecureElementPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, Package, Result,
    Trailing, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most random bytes one call returns.
const MAX_RANDOM_BYTES: usize = 1024;

/// Owns the Board's secure elements and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct SecureElementPlugin<Device> {
    elements: Vec<(&'static str, Device)>,
}

impl<Device> SecureElementPlugin<Device> {
    /// Takes the built-in secure elements from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: SecureElementPeripheral<SecureElement = Device>,
    {
        Self {
            elements: config::peripherals::SECURE_ELEMENT_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_secure_element(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for SecureElementPlugin<Device>
where
    Device: SecureElement + Send + 'static,
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
            .register(SecureElementPackage::new(core::mem::take(
                &mut self.elements,
            )))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The secure element lent to one call; it goes back when the call ends or is
/// cancelled.
struct Lent<Device>(LentDevice<Device>);

impl<Device> Lent<Device> {
    /// Lends the secure element `name`, or the primary one.
    fn take(
        devices: &BoardDevices<Device>,
        active: &AtomicBool,
        name: Option<&str>,
    ) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("secure_element package has been revoked"));
        }
        devices.lend(name).map(Self).ok_or_else(|| {
            Error::runtime(match name {
                Some(name) => alloc::format!("secure element `{name}` is unavailable or busy"),
                None => String::from("built-in secure element is unavailable or busy"),
            })
        })
    }

    fn device(&mut self) -> &mut Device {
        &mut self.0
    }
}

struct SecureElementPackage<Device> {
    elements: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> SecureElementPackage<Device> {
    fn new(elements: Vec<(&'static str, Device)>) -> Self {
        Self {
            elements: BoardDevices::new(elements),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn key_slot(slot: i64) -> Result<KeySlot> {
    u8::try_from(slot)
        .map(KeySlot::new)
        .map_err(|_| Error::runtime("a key slot is 0 through 255"))
}

fn element_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("secure element operation failed: {error:?}"))
}

/// The four calls, each with its own handles on the slot.
type Handles<Device> = (BoardDevices<Device>, Arc<AtomicBool>);

impl<Device> Package for SecureElementPackage<Device>
where
    Device: SecureElement + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let handles = || -> Handles<Device> { (self.elements.clone(), Arc::clone(&self.active)) };
        let available = handles();
        let list = self.elements.clone();
        let serial = handles();
        let random = handles();
        let public_key = handles();
        let sign = handles();
        lua.register_lib("secure_element", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (elements, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && elements.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_async("serial_number", move |name: Variadic<String>| {
                let (slot, active) = (serial.0.clone(), Arc::clone(&serial.1));
                let name = name.first().cloned();
                async move {
                    Some(
                        async {
                            let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                            let serial =
                                lent.device().serial_number().await.map_err(element_error)?;
                            Bytes::copy_from(serial.as_bytes())
                        }
                        .await,
                    )
                }
            })?;
            package.register_async(
                "random",
                move |Trailing((length,), name): Trailing<(i64,), String>| {
                    let (slot, active) = (random.0.clone(), Arc::clone(&random.1));
                    let parsed = usize::try_from(length)
                        .ok()
                        .filter(|length| (1..=MAX_RANDOM_BYTES).contains(length))
                        .ok_or_else(|| Error::runtime("a random length is 1 through 1024 bytes"));
                    async move {
                        Some(
                            async {
                                let mut bytes = Bytes::zeroed(parsed?)?;
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                lent.device()
                                    .fill_random(&mut bytes)
                                    .await
                                    .map_err(element_error)?;
                                Ok(bytes)
                            }
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "public_key",
                move |Trailing((slot_index,), name): Trailing<(i64,), String>| {
                    let (slot, active) = (public_key.0.clone(), Arc::clone(&public_key.1));
                    let parsed = key_slot(slot_index);
                    async move {
                        Some(
                            async {
                                let key_slot = parsed?;
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                let key = lent
                                    .device()
                                    .public_key(key_slot)
                                    .await
                                    .map_err(element_error)?;
                                Bytes::copy_from(key.as_bytes())
                            }
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "sign",
                move |Trailing((slot_index, digest), name): Trailing<(i64, Bytes), String>| {
                    let (slot, active) = (sign.0.clone(), Arc::clone(&sign.1));
                    let parsed = key_slot(slot_index).and_then(|key_slot| {
                        let digest: [u8; 32] = (*digest)
                            .try_into()
                            .map_err(|_| Error::runtime("a digest is exactly 32 bytes"))?;
                        Ok((key_slot, digest))
                    });
                    async move {
                        Some(
                            async {
                                let (key_slot, digest) = parsed?;
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                let signature = lent
                                    .device()
                                    .sign_digest(key_slot, &digest)
                                    .await
                                    .map_err(element_error)?;
                                Bytes::copy_from(signature.as_bytes())
                            }
                            .await,
                        )
                    }
                },
            )
        })
    }
}

impl<Device> LuaPackage for SecureElementPackage<Device>
where
    Device: SecureElement + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "secure_element"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.elements.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "硬件密钥签名与随机数",
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
    use barracuda_board_hal::secure_element::{P256PublicKey, P256Signature, SerialNumber};
    use core::convert::Infallible;

    /// Answers with fixed, recognizable values.
    struct TestElement;

    impl SecureElement for TestElement {
        type Error = Infallible;

        async fn serial_number(&mut self) -> core::result::Result<SerialNumber, Infallible> {
            Ok(SerialNumber::new(&[0x01, 0x23, 0xee]).expect("short"))
        }

        async fn fill_random(&mut self, buffer: &mut [u8]) -> core::result::Result<(), Infallible> {
            buffer.fill(0x5a);
            Ok(())
        }

        async fn public_key(
            &mut self,
            slot: KeySlot,
        ) -> core::result::Result<P256PublicKey, Infallible> {
            Ok(P256PublicKey::new([slot.index(); 64]))
        }

        async fn sign_digest(
            &mut self,
            slot: KeySlot,
            digest: &[u8; 32],
        ) -> core::result::Result<P256Signature, Infallible> {
            let mut signature = [slot.index(); 64];
            signature[..32].copy_from_slice(digest);
            Ok(P256Signature::new(signature))
        }
    }

    fn run(package: &SecureElementPackage<TestElement>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_reads_the_serial_random_bytes_keys_and_signatures() {
        let package = SecureElementPackage::new(std::vec![("main", TestElement)]);
        assert!(run(
            &package,
            "local se = require('secure_element')\n\
             assert(se.available())\n\
             assert(se.serial_number() == '\\x01\\x23\\xee')\n\
             assert(se.random(5) == 'ZZZZZ')\n\
             assert(se.public_key(2) == string.rep('\\2', 64))\n\
             local digest = string.rep('d', 32)\n\
             assert(se.sign(1, digest) == digest .. string.rep('\\1', 32))\n\
             local ok, err = se.sign(0, 'short')\n\
             assert(ok == nil and err:find('32 bytes') ~= nil)\n\
             ok, err = se.random(0)\n\
             return ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn lua_names_an_element_and_defaults_to_the_primary() {
        let package =
            SecureElementPackage::new(std::vec![("main", TestElement), ("spare", TestElement)]);
        assert!(run(
            &package,
            "local se = require('secure_element')\n\
             local names = se.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'spare')\n\
             assert(se.available('spare'))\n\
             assert(se.serial_number('spare') == '\\x01\\x23\\xee')\n\
             assert(se.random(2, 'spare') == 'ZZ')\n\
             assert(se.public_key(3, 'spare') == string.rep('\\3', 64))\n\
             assert(#se.sign(1, string.rep('d', 32), 'spare') == 64)\n\
             local ok, err = se.random(2, 'missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
    }

    #[test]
    fn a_board_without_secure_element_reports_unavailable() {
        let package = SecureElementPackage::<TestElement>::new(Vec::new());
        assert!(run(
            &package,
            "local se = require('secure_element')\n\
             local ok, err = se.serial_number()\n\
             return not se.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = SecureElementPackage::new(std::vec![("main", TestElement)]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
