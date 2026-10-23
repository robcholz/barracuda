//! Lua package for the Board's built-in LoRa radios.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::lora::{
    Bandwidth, CodingRate, Lora, LoraConfig, LoraPeripheral, MAX_PAYLOAD_BYTES,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue, Package,
    Result, Table, Trailing, Variadic,
};
use embassy_time::{Duration, with_timeout};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Owns the Board's LoRa radios and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct LoraPlugin<Device> {
    radios: Vec<(&'static str, Device)>,
}

impl<Device> LoraPlugin<Device> {
    /// Takes the built-in LoRa radios from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: LoraPeripheral<Lora = Device>,
    {
        Self {
            radios: config::peripherals::LORA_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_lora(*instance)
                        .map(|device| (*name, device))
                })
                .collect(),
        }
    }
}

impl<Device> Plugin for LoraPlugin<Device>
where
    Device: Lora + Send + 'static,
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
            .register(LoraPackage::new(core::mem::take(&mut self.radios)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// The LoRa radio lent to one call; it goes back when the call ends or is
/// cancelled.
struct Lent<Device>(LentDevice<Device>);

impl<Device> Lent<Device> {
    /// Lends the LoRa radio `name`, or the primary one.
    fn take(
        devices: &BoardDevices<Device>,
        active: &AtomicBool,
        name: Option<&str>,
    ) -> Result<Self> {
        if !active.load(Ordering::Acquire) {
            return Err(Error::runtime("lora package has been revoked"));
        }
        devices.lend(name).map(Self).ok_or_else(|| {
            Error::runtime(match name {
                Some(name) => alloc::format!("LoRa radio `{name}` is unavailable or busy"),
                None => String::from("built-in LoRa radio is unavailable or busy"),
            })
        })
    }

    fn device(&mut self) -> &mut Device {
        &mut self.0
    }
}

struct LoraPackage<Device> {
    radios: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> LoraPackage<Device> {
    fn new(radios: Vec<(&'static str, Device)>) -> Self {
        Self {
            radios: BoardDevices::new(radios),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

fn radio_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(alloc::format!("LoRa operation failed: {error:?}"))
}

/// One optional integer field of the settings table, in range.
fn field<T: TryFrom<i64>>(settings: &Table, name: &str, default: T) -> Result<T> {
    match settings.get::<_, Option<i64>>(name)? {
        None => Ok(default),
        Some(value) => T::try_from(value)
            .map_err(|_| Error::runtime(alloc::format!("LoRa setting `{name}` is out of range"))),
    }
}

/// Reads `{frequency_hz = ..., spreading_factor = 8, bandwidth_hz = 250000,
/// coding_rate = 8, power_dbm = 10, preamble = 12, sync_word = 0x12,
/// crc = true}`; only the frequency is required.
fn settings(table: &Table) -> Result<LoraConfig> {
    let frequency_hz = table
        .get::<_, Option<i64>>("frequency_hz")?
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Error::runtime("LoRa setting `frequency_hz` is required"))?;
    let defaults = LoraConfig::new(frequency_hz);
    let bandwidth_hz = field(table, "bandwidth_hz", defaults.bandwidth.hz())?;
    let coding_rate = field(table, "coding_rate", defaults.coding_rate.denominator())?;
    Ok(LoraConfig {
        frequency_hz,
        spreading_factor: field(table, "spreading_factor", defaults.spreading_factor)?,
        bandwidth: Bandwidth::from_hz(bandwidth_hz)
            .ok_or_else(|| Error::runtime("LoRa `bandwidth_hz` is not a LoRa bandwidth"))?,
        coding_rate: CodingRate::from_denominator(coding_rate)
            .ok_or_else(|| Error::runtime("LoRa `coding_rate` is 5 through 8, for 4/5 to 4/8"))?,
        tx_power_dbm: field(table, "power_dbm", defaults.tx_power_dbm)?,
        preamble_symbols: field(table, "preamble", defaults.preamble_symbols)?,
        sync_word: field(table, "sync_word", defaults.sync_word)?,
        crc: table.get::<_, Option<bool>>("crc")?.unwrap_or(defaults.crc),
    })
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
                "expected an optional timeout in milliseconds and radio name",
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
        _ => return Err(Error::runtime("expected a LoRa radio name")),
    };
    Ok((limit, name))
}

/// A packet's payload, RSSI in dBm, and SNR in dB; or nil and "timeout".
type ReceiveResult = (Option<Bytes>, Option<LuaValue>, Option<f64>);

impl<Device> Package for LoraPackage<Device>
where
    Device: Lora + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.radios.clone(), Arc::clone(&self.active));
        let list = self.radios.clone();
        let limits = (self.radios.clone(), Arc::clone(&self.active));
        let configure = (self.radios.clone(), Arc::clone(&self.active));
        let send = (self.radios.clone(), Arc::clone(&self.active));
        let receive = (self.radios.clone(), Arc::clone(&self.active));
        lua.register_lib("lora", move |package| {
            package.register("available", move |name: Variadic<String>| {
                let (radios, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && radios.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_with("limits", move |lua, name: Variadic<String>| {
                let (slot, active) = &limits;
                Some((|| {
                    let range = Lent::take(slot, active, name.first().map(String::as_str))?
                        .device()
                        .limits();
                    let table = lua.create_table()?;
                    table.set("min_frequency_hz", i64::from(range.min_frequency_hz))?;
                    table.set("max_frequency_hz", i64::from(range.max_frequency_hz))?;
                    table.set("min_power_dbm", i64::from(range.min_tx_power_dbm))?;
                    table.set("max_power_dbm", i64::from(range.max_tx_power_dbm))?;
                    Ok(table)
                })())
            })?;
            package.register_async(
                "configure",
                move |Trailing((table,), name): Trailing<(Table,), String>| {
                    let (slot, active) = (configure.0.clone(), Arc::clone(&configure.1));
                    let parsed = settings(&table);
                    async move {
                        Some(
                            async {
                                let config = parsed?;
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                lent.device().configure(config).await.map_err(radio_error)?;
                                Ok(true)
                            }
                            .await,
                        )
                    }
                },
            )?;
            package.register_async(
                "send",
                move |Trailing((payload,), name): Trailing<(Bytes,), String>| {
                    let (slot, active) = (send.0.clone(), Arc::clone(&send.1));
                    async move {
                        Some(
                            async {
                                if payload.is_empty() || payload.len() > MAX_PAYLOAD_BYTES {
                                    return Err(Error::runtime(
                                        "a LoRa packet carries 1 to 255 bytes",
                                    ));
                                }
                                let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                                lent.device()
                                    .transmit(&payload)
                                    .await
                                    .map_err(radio_error)?;
                                Ok(true)
                            }
                            .await,
                        )
                    }
                },
            )?;
            package.register_async("receive", move |values: Variadic<LuaValue>| {
                let (slot, active) = (receive.0.clone(), Arc::clone(&receive.1));
                let limit = timeout(values);
                async move {
                    Some(
                        async {
                            let (limit, name) = limit?;
                            let mut lent = Lent::take(&slot, &active, name.as_deref())?;
                            let device = lent.device();
                            let mut buffer = Bytes::zeroed(MAX_PAYLOAD_BYTES)?;
                            let received = match limit {
                                None => Some(device.receive(&mut buffer).await),
                                Some(limit) => {
                                    with_timeout(limit, device.receive(&mut buffer)).await.ok()
                                }
                            };
                            let Some(received) = received else {
                                return Ok((
                                    None,
                                    Some(LuaValue::String(String::from("timeout"))),
                                    None,
                                ));
                            };
                            let received = received.map_err(radio_error)?;
                            buffer.truncate(received.len);
                            Ok((
                                Some(buffer),
                                Some(LuaValue::Integer(i64::from(received.rssi_dbm))),
                                Some(f64::from(received.snr_centi_db) / 100.0),
                            ))
                        }
                        .await,
                    ) as Option<Result<ReceiveResult>>
                }
            })
        })
    }
}

impl<Device> LuaPackage for LoraPackage<Device>
where
    Device: Lora + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "lora"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        (!self.radios.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 2,
            purpose: "LoRa 无线收发",
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
    use barracuda_board_hal::lora::{RadioLimits, Received};

    /// A radio that hears back whatever it sends.
    #[derive(Default)]
    struct Echo {
        config: Option<LoraConfig>,
        last: Vec<u8>,
    }

    impl Lora for Echo {
        type Error = &'static str;

        fn limits(&self) -> RadioLimits {
            RadioLimits {
                min_frequency_hz: 850_000_000,
                max_frequency_hz: 930_000_000,
                min_tx_power_dbm: -9,
                max_tx_power_dbm: 22,
            }
        }

        async fn configure(&mut self, config: LoraConfig) -> core::result::Result<(), Self::Error> {
            self.config = Some(config);
            Ok(())
        }

        async fn transmit(&mut self, payload: &[u8]) -> core::result::Result<(), Self::Error> {
            self.config.ok_or("not configured")?;
            self.last = payload.to_vec();
            Ok(())
        }

        async fn receive(
            &mut self,
            buffer: &mut [u8],
        ) -> core::result::Result<Received, Self::Error> {
            if self.last.is_empty() {
                // Nothing on the air: never returns.
                core::future::pending::<()>().await;
            }
            let len = self.last.len().min(buffer.len());
            buffer[..len].copy_from_slice(&self.last[..len]);
            Ok(Received {
                len,
                rssi_dbm: -61,
                snr_centi_db: 725,
            })
        }
    }

    fn run(package: &LoraPackage<Echo>, script: &str) -> bool {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        futures_lite::future::block_on(lua.load(script).eval_async()).expect("run script")
    }

    #[test]
    fn lua_configures_sends_and_receives() {
        let package = LoraPackage::new(std::vec![("main", Echo::default())]);
        assert!(run(
            &package,
            "local lora = require('lora')\n\
             assert(lora.available())\n\
             assert(lora.limits().max_frequency_hz == 930000000)\n\
             local ok, err = lora.configure({ bandwidth_hz = 125000 })\n\
             assert(ok == nil and err:find('frequency_hz'))\n\
             ok, err = lora.configure({ frequency_hz = 868000000, bandwidth_hz = 123 })\n\
             assert(ok == nil and err:find('bandwidth'))\n\
             assert(lora.configure({ frequency_hz = 868000000, spreading_factor = 9, bandwidth_hz = 125000, coding_rate = 5 }))\n\
             assert(lora.send('ping'))\n\
             local data, rssi, snr = lora.receive(1000)\n\
             return data == 'ping' and rssi == -61 and snr == 7.25",
        ));
        let echo = package.radios.take(None).expect("radio back in its slot");
        let config = echo.config.expect("configured");
        assert_eq!(config.spreading_factor, 9);
        assert_eq!(config.bandwidth, Bandwidth::Khz125);
        assert_eq!(config.coding_rate, CodingRate::Cr4_5);
        assert_eq!(config.sync_word, 0x12);
    }

    #[test]
    fn lua_names_a_radio_and_defaults_to_the_primary() {
        let package = LoraPackage::new(std::vec![
            ("main", Echo::default()),
            ("side", Echo::default()),
        ]);
        assert!(run(
            &package,
            "local lora = require('lora')\n\
             local names = lora.list()\n\
             assert(#names == 2 and names[1] == 'main' and names[2] == 'side')\n\
             assert(lora.limits('side').min_power_dbm == -9)\n\
             assert(lora.configure({ frequency_hz = 868000000 }, 'side'))\n\
             assert(lora.send('pong', 'side'))\n\
             local data = lora.receive(1000, 'side')\n\
             assert(data == 'pong')\n\
             local ok, err = lora.send('ping')\n\
             assert(ok == nil and err:find('not configured'))\n\
             ok, err = lora.send('ping', 'missing')\n\
             return ok == nil and err:find('missing') ~= nil",
        ));
    }

    #[test]
    fn receive_times_out() {
        let package = LoraPackage::new(std::vec![("main", Echo::default())]);
        assert!(run(
            &package,
            "local lora = require('lora')\n\
             local data, err = lora.receive(10)\n\
             return data == nil and err == 'timeout'",
        ));
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = LoraPackage::new(std::vec![("main", Echo::default())]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
