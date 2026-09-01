//! Deterministic host Platform used by the end-to-end composition root.

use std::vec;
use std::vec::Vec;

use barracuda_platform::{
    NamedPartition, PartitionAccess, Partitions, PartitionsInsertError, Platform,
    PlatformInitResult, PlatformResources,
};
use barracuda_tls::PlaintextTls;
use embassy_executor::{SpawnError, Spawner};
use embassy_net::Ipv4Address;
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use crate::network::{loopback_network_with_dns, LoopbackNetwork};

pub(crate) const PARTITION_CAPACITY: usize = 3;
const SYSTEM_PARTITION_BYTES: usize = 1024 * 1024;
const KV_PARTITION_BYTES: usize = 256 * 1024;
const WEB_PARTITION_BYTES: usize = 64 * 1024;

/// Bindings owned by the e2e Platform before initialization.
pub struct E2ePlatformBindings {
    network: LoopbackNetwork,
}

impl E2ePlatformBindings {
    /// Creates a fresh in-process Embassy network binding.
    #[must_use]
    pub fn new() -> Self {
        Self {
            network: loopback_network_with_dns(Some(Ipv4Address::new(10, 0, 0, 1))),
        }
    }
}

impl Default for E2ePlatformBindings {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic host Platform used only by the e2e composition root.
pub struct E2ePlatform;

impl Platform for E2ePlatform {
    type Bindings = E2ePlatformBindings;
    type Tls = PlaintextTls;
    type Partitions = Partitions<MemoryNorFlash, PARTITION_CAPACITY>;
    type Error = E2ePlatformError;

    async fn initialize(spawner: Spawner, bindings: Self::Bindings) -> PlatformInitResult<Self> {
        let stack = bindings.network.stack();
        spawner.spawn(run_network(bindings.network))?;

        let mut partitions = Partitions::new();
        partitions.insert(NamedPartition::new(
            "system",
            PartitionAccess::ReadWrite,
            MemoryNorFlash::new(SYSTEM_PARTITION_BYTES),
        ))?;
        partitions.insert(NamedPartition::new(
            "kv_database",
            PartitionAccess::ReadWrite,
            MemoryNorFlash::new(KV_PARTITION_BYTES),
        ))?;
        partitions.insert(NamedPartition::new(
            "web_assets",
            PartitionAccess::ReadOnly,
            MemoryNorFlash::new(WEB_PARTITION_BYTES),
        ))?;

        Ok(PlatformResources {
            ip_stack: stack,
            tls: PlaintextTls,
            partitions,
        })
    }
}

#[embassy_executor::task(pool_size = 2)]
async fn run_network(network: LoopbackNetwork) {
    network.run().await;
}

/// Failure while initializing e2e Platform mechanisms.
#[derive(Debug, thiserror::Error)]
pub enum E2ePlatformError {
    /// The permanent Embassy network runner could not be spawned.
    #[error("failed to spawn e2e Platform network runner: {0}")]
    Spawn(#[from] SpawnError),
    /// The fixed partition collection rejected a required native region.
    #[error("failed to construct e2e native partitions: {0}")]
    Partitions(#[from] PartitionsInsertError),
}

/// Volatile blocking NOR region used by the e2e Platform.
pub struct MemoryNorFlash {
    bytes: Vec<u8>,
}

impl MemoryNorFlash {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
        }
    }

    fn range(
        &self,
        offset: u32,
        length: usize,
        alignment: usize,
    ) -> Result<core::ops::Range<usize>, MemoryNorFlashError> {
        let start = usize::try_from(offset).map_err(|_error| MemoryNorFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(MemoryNorFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        Ok(start..end)
    }
}

/// Failure from the e2e Platform's volatile NOR region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryNorFlashError {
    /// An operation exceeded the allocated region.
    OutOfBounds,
    /// An operation violated the region's declared granularity.
    NotAligned,
}

impl core::fmt::Display for MemoryNorFlashError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds => formatter.write_str("mock NOR access is out of bounds"),
            Self::NotAligned => formatter.write_str("mock NOR access is not aligned"),
        }
    }
}

impl std::error::Error for MemoryNorFlashError {}

impl NorFlashError for MemoryNorFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

impl ErrorType for MemoryNorFlash {
    type Error = MemoryNorFlashError;
}

impl ReadNorFlash for MemoryNorFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::READ_SIZE)?;
        let source = self
            .bytes
            .get(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for MemoryNorFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = to
            .checked_sub(from)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        self.bytes
            .get_mut(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let target = self
            .bytes
            .get_mut(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        for (target, source) in target.iter_mut().zip(bytes) {
            *target &= *source;
        }
        Ok(())
    }
}
