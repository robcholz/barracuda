//! Lua package for the Board's primary built-in secure element.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use barracuda_board_hal::secure_element::{KeySlot, SecureElement, SecureElementPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Most random bytes one call returns.
const MAX_RANDOM_BYTES: usize = 1024;

/// Owns the Board's primary secure element and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct SecureElementPlugin<Device> {
    element: Option<Device>,
}

impl<Device> SecureElementPlugin<Device> {
    /// Takes the built-in secure element from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: SecureElementPeripheral<SecureElement = Device>,
    {
        Self {
            element: config::peripherals::SECURE_ELEMENT_DEVICE
                .and_then(|instance| context.hal.peripherals.take_secure_element(instance)),
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
            .register(SecureElementPackage::new(self.element.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The element while no call uses it.
type Slot<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The element taken out of its slot for one call; it goes back when the
/// call ends or is cancelled.
struct Lent<Device> {
    device: Option<Device>,
    home: Slot<Device>,
}

impl<Device> Lent<Device> {
    fn take(home: &Slot<Device>, active: &AtomicBool) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("secure_element package has been revoked"));
        }
        let device = critical_section::with(|section| home.borrow(section).borrow_mut().take());
        match device {
            Some(device) => Ok(Self {
                device: Some(device),
                home: Arc::clone(home),
            }),
            None => Err(Error::runtime(
                "built-in secure element is unavailable or busy",
            )),
        }
    }

    fn device(&mut self) -> Result<&mut Device> {
        self.device
            .as_mut()
            .ok_or_else(|| Error::runtime("built-in secure element is unavailable or busy"))
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

struct SecureElementPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    element: Slot<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> SecureElementPackage<Device> {
    fn new(element: Option<Device>) -> Self {
        Self {
            present: element.is_some(),
            element: Arc::new(critical_section::Mutex::new(RefCell::new(element))),
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
type Handles<Device> = (Slot<Device>, Arc<AtomicBool>);

impl<Device> Package for SecureElementPackage<Device>
where
    Device: SecureElement + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let handles =
            || -> Handles<Device> { (Arc::clone(&self.element), Arc::clone(&self.active)) };
        let available = handles();
        let serial = handles();
        let random = handles();
        let public_key = handles();
        let sign = handles();
        lua.register_lib("secure_element", move |package| {
            package.register("available", move |(): ()| {
                let (slot, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        slot.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_async("serial_number", move |(): ()| {
                let (slot, active) = (Arc::clone(&serial.0), Arc::clone(&serial.1));
                async move {
                    Some(
                        async {
                            let mut lent = Lent::take(&slot, &active)?;
                            let serial = lent
                                .device()?
                                .serial_number()
                                .await
                                .map_err(element_error)?;
                            Bytes::copy_from(serial.as_bytes())
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("random", move |length: i64| {
                let (slot, active) = (Arc::clone(&random.0), Arc::clone(&random.1));
                let parsed = usize::try_from(length)
                    .ok()
                    .filter(|length| (1..=MAX_RANDOM_BYTES).contains(length))
                    .ok_or_else(|| Error::runtime("a random length is 1 through 1024 bytes"));
                async move {
                    Some(
                        async {
                            let mut bytes = Bytes::zeroed(parsed?)?;
                            let mut lent = Lent::take(&slot, &active)?;
                            lent.device()?
                                .fill_random(&mut bytes)
                                .await
                                .map_err(element_error)?;
                            Ok(bytes)
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("public_key", move |slot_index: i64| {
                let (slot, active) = (Arc::clone(&public_key.0), Arc::clone(&public_key.1));
                let parsed = key_slot(slot_index);
                async move {
                    Some(
                        async {
                            let key_slot = parsed?;
                            let mut lent = Lent::take(&slot, &active)?;
                            let key = lent
                                .device()?
                                .public_key(key_slot)
                                .await
                                .map_err(element_error)?;
                            Bytes::copy_from(key.as_bytes())
                        }
                        .await,
                    )
                }
            })?;
            package.register_async("sign", move |(slot_index, digest): (i64, Bytes)| {
                let (slot, active) = (Arc::clone(&sign.0), Arc::clone(&sign.1));
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
                            let mut lent = Lent::take(&slot, &active)?;
                            let signature = lent
                                .device()?
                                .sign_digest(key_slot, &digest)
                                .await
                                .map_err(element_error)?;
                            Bytes::copy_from(signature.as_bytes())
                        }
                        .await,
                    )
                }
            })
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
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 1,
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
        let package = SecureElementPackage::new(Some(TestElement));
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
    fn a_board_without_secure_element_reports_unavailable() {
        let package = SecureElementPackage::<TestElement>::new(None);
        assert!(run(
            &package,
            "local se = require('secure_element')\n\
             local ok, err = se.serial_number()\n\
             return not se.available() and ok == nil and type(err) == 'string'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = SecureElementPackage::new(Some(TestElement));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
