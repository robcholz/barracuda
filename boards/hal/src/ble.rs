//! Platform-independent Bluetooth Low Energy runtime contracts.

use core::{convert::Infallible, future::Future};

/// Maximum payload carried by a legacy BLE advertisement.
pub const LEGACY_ADVERTISEMENT_MAX_BYTES: usize = 31;

/// Kind of Bluetooth device address reported by a scanner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BleAddressKind {
    /// Public address assigned by the IEEE registration authority.
    #[default]
    Public,
    /// Random static or private address.
    Random,
}

/// One Bluetooth device address in canonical display order.
///
/// `bytes[0]` is the most-significant octet rendered first in a conventional
/// colon-separated address. Platform adapters convert vendor-specific byte
/// order before constructing this value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BleAddress {
    /// Six address bytes in canonical display order.
    pub bytes: [u8; 6],
    /// Address classification required when establishing a connection.
    pub kind: BleAddressKind,
}

/// A bounded advertisement produced by a BLE scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BleAdvertisement {
    /// Address of the advertising device.
    pub address: BleAddress,
    /// Received signal strength in dBm.
    pub rssi_dbm: i8,
    /// Whether the advertising device accepts connections.
    pub connectable: bool,
    payload: [u8; LEGACY_ADVERTISEMENT_MAX_BYTES],
    payload_len: u8,
}

impl BleAdvertisement {
    /// Creates an advertisement from one bounded legacy advertising payload.
    ///
    /// # Errors
    ///
    /// Returns [`BleAdvertisementError`] when `payload` exceeds the legacy
    /// advertising limit.
    pub fn new(
        address: BleAddress,
        rssi_dbm: i8,
        connectable: bool,
        payload: &[u8],
    ) -> Result<Self, BleAdvertisementError> {
        if payload.len() > LEGACY_ADVERTISEMENT_MAX_BYTES {
            return Err(BleAdvertisementError::PayloadTooLong);
        }
        let payload_len =
            u8::try_from(payload.len()).map_err(|_| BleAdvertisementError::PayloadTooLong)?;
        let mut stored = [0; LEGACY_ADVERTISEMENT_MAX_BYTES];
        stored[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            address,
            rssi_dbm,
            connectable,
            payload: stored,
            payload_len,
        })
    }

    /// Returns the received advertising data.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }
}

/// Failure while constructing bounded advertisement metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BleAdvertisementError {
    /// Advertising data exceeds the legacy 31-byte payload.
    PayloadTooLong,
}

impl core::fmt::Display for BleAdvertisementError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PayloadTooLong => formatter.write_str("BLE advertisement exceeds 31 bytes"),
        }
    }
}

impl core::error::Error for BleAdvertisementError {}

/// Configuration for one bounded BLE scan operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BleScanRequest {
    /// Use active scanning and request scan responses when supported.
    pub active: bool,
    /// Maximum time to wait for one advertisement.
    pub timeout_millis: u32,
}

/// Platform-independent BLE adapter exposed to one application handle.
pub trait BleAdapter: Send + 'static {
    /// Failure reported by the concrete BLE stack.
    type Error: core::error::Error;

    /// Waits for one advertisement, returning `None` when the timeout expires.
    fn scan<'a>(
        &'a mut self,
        request: BleScanRequest,
    ) -> impl Future<Output = Result<Option<BleAdvertisement>, Self::Error>> + 'a;
}

/// Constructs one exclusive BLE adapter from the runtime hardware owner.
pub trait BleProvider {
    /// Adapter held by the VM BLE handle.
    type Adapter: BleAdapter;
    /// Failure while claiming the singleton BLE adapter.
    type Error: core::error::Error;

    /// Returns whether the adapter is present and has not been claimed.
    fn ble_available(&self) -> bool;

    /// Claims the singleton BLE adapter for this boot.
    fn take_ble(&self) -> Result<Self::Adapter, Self::Error>;
}

/// Failure while claiming the singleton BLE adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BleOpenError {
    /// The selected target has no BLE adapter.
    Unavailable,
    /// The BLE adapter was already claimed during this boot.
    Busy,
}

impl core::fmt::Display for BleOpenError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("BLE is unavailable on the selected target"),
            Self::Busy => formatter.write_str("BLE was already opened during this boot"),
        }
    }
}

impl core::error::Error for BleOpenError {}

/// Uninhabited adapter used by targets without BLE support.
pub struct UnavailableBleAdapter {
    never: Infallible,
}

impl BleAdapter for UnavailableBleAdapter {
    type Error = Infallible;

    async fn scan(
        &mut self,
        _request: BleScanRequest,
    ) -> Result<Option<BleAdvertisement>, Self::Error> {
        match self.never {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn advertisement_preserves_a_bounded_payload() {
        let payload = [0x5a; LEGACY_ADVERTISEMENT_MAX_BYTES];
        let advertisement = BleAdvertisement::new(
            BleAddress {
                bytes: [1, 2, 3, 4, 5, 6],
                kind: BleAddressKind::Random,
            },
            -60,
            true,
            &payload,
        )
        .expect("legacy payload fits");

        assert_eq!(advertisement.payload(), payload);
    }

    #[test]
    fn advertisement_rejects_an_oversized_payload() {
        let payload = [0; LEGACY_ADVERTISEMENT_MAX_BYTES + 1];

        assert_eq!(
            BleAdvertisement::new(BleAddress::default(), 0, false, &payload),
            Err(BleAdvertisementError::PayloadTooLong)
        );
    }
}
