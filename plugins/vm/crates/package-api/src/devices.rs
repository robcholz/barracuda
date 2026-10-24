//! The Board devices of one kind a Lua package publishes by name.

use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};

use barracuda_lua::{Context, Result as LuaResult, Table};
use portable_atomic_util::Arc;
use spin::Mutex;

/// Each device by its Board name, `None` while a script holds it.
type Slots<Device> = Arc<Mutex<Vec<(&'static str, Option<Device>)>>>;

/// The Board devices of one kind a Lua package publishes, by Board name.
///
/// A package receives its devices from a `"*"` peripheral slot, which lists
/// the Board's primary device first. A script that names no device gets that
/// first one, so a script written for a Board with one device works on a
/// Board with several. Clones share the same devices.
pub struct BoardDevices<Device> {
    slots: Slots<Device>,
}

impl<Device> Clone for BoardDevices<Device> {
    fn clone(&self) -> Self {
        Self {
            slots: Arc::clone(&self.slots),
        }
    }
}

impl<Device> BoardDevices<Device> {
    /// Publishes `devices` by Board name; the first is the default.
    pub fn new(devices: impl IntoIterator<Item = (&'static str, Device)>) -> Self {
        Self {
            slots: Arc::new(Mutex::new(
                devices
                    .into_iter()
                    .map(|(name, device)| (name, Some(device)))
                    .collect(),
            )),
        }
    }

    /// Returns whether the package was given no device at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.lock().is_empty()
    }

    /// Returns the Board names of the devices, the default first.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.slots.lock().iter().map(|(name, _)| *name).collect()
    }

    /// Builds the Lua list of the devices' Board names, the default first.
    ///
    /// # Errors
    ///
    /// Returns the Lua error of a table that cannot be built.
    pub fn list(&self, lua: &mut Context<'_>) -> LuaResult<Table> {
        let table = lua.create_table()?;
        for (index, name) in (1_i64..).zip(self.names()) {
            table.set(index, name)?;
        }
        Ok(table)
    }

    /// Returns whether the device named `name`, or the default device when
    /// `name` is `None`, exists and no script holds it.
    #[must_use]
    pub fn is_available(&self, name: Option<&str>) -> bool {
        let slots = self.slots.lock();
        position(&slots, name).is_some_and(|index| slots[index].1.is_some())
    }

    /// Moves the device named `name`, or the default device, out for good;
    /// `None` when there is no such device or a script holds it.
    #[must_use]
    pub fn take(&self, name: Option<&str>) -> Option<Device> {
        let mut slots = self.slots.lock();
        let index = position(&slots, name)?;
        slots[index].1.take()
    }

    /// Lends the device named `name`, or the default device, until the
    /// returned guard drops; `None` when there is no such device or a script
    /// holds it.
    #[must_use]
    pub fn lend(&self, name: Option<&str>) -> Option<LentDevice<Device>> {
        let mut slots = self.slots.lock();
        let index = position(&slots, name)?;
        let device = slots[index].1.take()?;
        Some(LentDevice {
            device: Some(device),
            index,
            home: Arc::clone(&self.slots),
        })
    }
}

/// The index of the device named `name`, or of the default device.
fn position<Device>(slots: &[(&'static str, Option<Device>)], name: Option<&str>) -> Option<usize> {
    match name {
        None => (!slots.is_empty()).then_some(0),
        Some(name) => slots.iter().position(|(device, _)| *device == name),
    }
}

/// A device a script holds; it goes back to its [`BoardDevices`] when the
/// guard drops, however the call that holds it ends.
pub struct LentDevice<Device> {
    device: Option<Device>,
    index: usize,
    home: Slots<Device>,
}

impl<Device> Deref for LentDevice<Device> {
    type Target = Device;

    #[allow(clippy::expect_used, reason = "the device leaves only in drop")]
    fn deref(&self) -> &Device {
        self.device
            .as_ref()
            .expect("a lent device is held until drop")
    }
}

impl<Device> DerefMut for LentDevice<Device> {
    #[allow(clippy::expect_used, reason = "the device leaves only in drop")]
    fn deref_mut(&mut self) -> &mut Device {
        self.device
            .as_mut()
            .expect("a lent device is held until drop")
    }
}

impl<Device> Drop for LentDevice<Device> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            if let Some((_, slot)) = self.home.lock().get_mut(self.index) {
                *slot = Some(device);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BoardDevices;

    #[test]
    fn the_first_device_is_the_default_and_lent_devices_come_back() {
        let devices = BoardDevices::new([("front", 1_u8), ("back", 2_u8)]);
        assert_eq!(devices.names(), ["front", "back"]);
        assert!(devices.is_available(None));

        let front = devices.lend(None).map(|device| *device);
        assert_eq!(front, Some(1));
        let held = devices.lend(Some("back"));
        assert_eq!(held.as_deref(), Some(&2));
        assert!(!devices.is_available(Some("back")));
        assert!(devices.lend(Some("back")).is_none());
        drop(held);
        assert!(devices.is_available(Some("back")));

        assert_eq!(devices.take(None), Some(1));
        assert!(!devices.is_available(None));
        assert!(devices.take(None).is_none());
        assert!(!devices.is_available(Some("side")));
        assert!(!devices.is_empty());
        assert!(BoardDevices::<u8>::new([]).is_empty());
    }
}
