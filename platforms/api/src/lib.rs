//! Compile-time Platform resource contract.
//!
//! Platforms expose exact platform mechanisms. The current common contract is
//! one Embassy IP stack, one Wi-Fi mechanism, one entropy source, and one
//! generic collection of native partitions.

#![no_std]

extern crate alloc;

mod entropy;
mod network;
mod wifi;

use core::future::Future;

use embassy_executor::Spawner;
use embassy_net::Stack;

pub use entropy::{Entropy, EntropyUnavailable, UnavailableEntropy};
pub use network::{UnavailableNetworkDriver, UnavailableToken};
pub use wifi::{
    AccessPointConfiguration, AccessPointState, HostWifiDevice, StationConfiguration, StationState,
    UnavailableWifiDevice, VisibleNetwork, WifiCapabilities, WifiDevice,
};

/// Fixed identity of one compiled execution Platform.
///
/// Concrete Platform manifests provide these semantic values. The selected
/// Platform projection adds the final Cargo target architecture at build time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformInfo {
    name: &'static str,
    family: &'static str,
    architecture: &'static str,
    environment: &'static str,
    long_lived_connections: ConnectionBudget,
}

impl PlatformInfo {
    /// Creates one static Platform descriptor.
    #[must_use]
    pub const fn new(
        name: &'static str,
        family: &'static str,
        architecture: &'static str,
        environment: &'static str,
    ) -> Self {
        Self {
            name,
            family,
            architecture,
            environment,
            long_lived_connections: ConnectionBudget::NONE,
        }
    }

    /// Declares how many long-lived connections this Platform's memory holds.
    #[must_use]
    pub const fn with_long_lived_connections(mut self, budget: ConnectionBudget) -> Self {
        self.long_lived_connections = budget;
        self
    }

    /// Returns the stable Platform bundle name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the semantic Platform family.
    #[must_use]
    pub const fn family(&self) -> &'static str {
        self.family
    }

    /// Returns the compiled Rust target architecture.
    #[must_use]
    pub const fn architecture(&self) -> &'static str {
        self.architecture
    }

    /// Returns the execution environment class.
    #[must_use]
    pub const fn environment(&self) -> &'static str {
        self.environment
    }

    /// Returns how many long-lived connections this Platform's memory holds.
    #[must_use]
    pub const fn long_lived_connections(&self) -> ConnectionBudget {
        self.long_lived_connections
    }
}

/// How many long-lived client connections (TCP socket plus TLS session, held
/// open by a receive loop) a Platform holds beside the shared request pool.
///
/// The figure depends on where TLS record buffers live: in internal RAM, or
/// in the external memory a Board declares when the Platform installs it as
/// bulk memory. A Platform without external-memory support declares the same
/// figure for both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionBudget {
    internal_memory: u8,
    external_memory: u8,
}

impl ConnectionBudget {
    /// No long-lived connections.
    pub const NONE: Self = Self::new(0, 0);

    /// Creates a budget for Boards without and with external memory.
    #[must_use]
    pub const fn new(internal_memory: u8, external_memory: u8) -> Self {
        Self {
            internal_memory,
            external_memory,
        }
    }

    /// Returns the connections held when bulk memory is internal RAM.
    #[must_use]
    pub const fn internal_memory(&self) -> u8 {
        self.internal_memory
    }

    /// Returns the connections held when the Board declares external memory.
    #[must_use]
    pub const fn external_memory(&self) -> u8 {
        self.external_memory
    }

    /// Returns the connections held on a Board with or without external
    /// memory.
    #[must_use]
    pub const fn for_board(&self, external_memory: bool) -> u8 {
        if external_memory {
            self.external_memory
        } else {
            self.internal_memory
        }
    }
}

/// Whether consumers may mutate one native partition at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionAccess {
    /// The native layout marks the partition as immutable at runtime.
    ReadOnly,
    /// The native layout permits erase and program operations.
    ReadWrite,
}

/// On-media format declared by the native partition table.
///
/// This is layout metadata, not content detection. Consumers select a driver
/// from this declaration and must not inspect partition bytes to guess it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionFilesystem {
    /// The partition has no filesystem contract.
    Raw,
    /// The partition contains a FAT filesystem.
    FatFs,
    /// The partition contains a LittleFS filesystem.
    LittleFs,
}

/// One named native partition and its platform-specific region handle.
pub struct NamedPartition<Region> {
    name: &'static str,
    access: PartitionAccess,
    filesystem: PartitionFilesystem,
    region: Region,
}

impl<Region> NamedPartition<Region> {
    /// Creates a partition entry from a validated native layout.
    #[must_use]
    pub const fn new(
        name: &'static str,
        access: PartitionAccess,
        filesystem: PartitionFilesystem,
        region: Region,
    ) -> Self {
        Self {
            name,
            access,
            filesystem,
            region,
        }
    }

    /// Returns the native partition name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the native runtime access discipline.
    #[must_use]
    pub const fn access(&self) -> PartitionAccess {
        self.access
    }

    /// Returns the on-media format declared by the native layout.
    #[must_use]
    pub const fn filesystem(&self) -> PartitionFilesystem {
        self.filesystem
    }

    /// Returns the platform-specific region handle.
    #[must_use]
    pub const fn region(&self) -> &Region {
        &self.region
    }

    /// Consumes the entry and returns its platform-specific region handle.
    #[must_use]
    pub fn into_region(self) -> Region {
        self.region
    }
}

/// Fixed-capacity collection of arbitrary native partitions.
///
/// Capacity is a compile-time storage bound, not a list of business roles.
/// Adding a consumer does not add a field to this type or to
/// [`PlatformResources`].
pub struct Partitions<Region, const N: usize> {
    entries: [Option<NamedPartition<Region>>; N],
    len: usize,
}

impl<Region, const N: usize> Partitions<Region, N> {
    /// Creates an empty partition collection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: core::array::from_fn(|_| None),
            len: 0,
        }
    }

    /// Inserts one native partition.
    ///
    /// # Errors
    ///
    /// Returns an error when the native name is empty or duplicated, or when
    /// the configured compile-time capacity has been exhausted.
    pub fn insert(
        &mut self,
        partition: NamedPartition<Region>,
    ) -> Result<(), PartitionsInsertError> {
        if partition.name().is_empty() {
            return Err(PartitionsInsertError::EmptyName);
        }
        if self.get(partition.name()).is_some() {
            return Err(PartitionsInsertError::DuplicateName);
        }
        let slot = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(PartitionsInsertError::Full)?;
        *slot = Some(partition);
        self.len = self.len.checked_add(1).ok_or(PartitionsInsertError::Full)?;
        Ok(())
    }

    /// Returns a borrowed partition by its native name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&NamedPartition<Region>> {
        self.entries
            .iter()
            .filter_map(Option::as_ref)
            .find(|partition| partition.name() == name)
    }

    /// Removes and returns a partition by its native name.
    pub fn take(&mut self, name: &str) -> Option<NamedPartition<Region>> {
        let entry = self.entries.iter_mut().find(|entry| {
            entry
                .as_ref()
                .is_some_and(|partition| partition.name() == name)
        })?;
        let partition = entry.take()?;
        self.len = self.len.saturating_sub(1);
        Some(partition)
    }

    /// Returns the number of available partitions.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no partitions remain available.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<Region, const N: usize> Default for Partitions<Region, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure while assembling a Platform partition collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionsInsertError {
    /// The native partition name is empty.
    EmptyName,
    /// The native layout contains the same partition name more than once.
    DuplicateName,
    /// The collection capacity is smaller than the native layout.
    Full,
}

impl core::fmt::Display for PartitionsInsertError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("partition name is empty"),
            Self::DuplicateName => formatter.write_str("partition name is duplicated"),
            Self::Full => formatter.write_str("partition collection is full"),
        }
    }
}

impl core::error::Error for PartitionsInsertError {}

/// Portable mechanisms produced by one concrete Platform.
pub struct PlatformResources<Partitions, Wifi, Entropy> {
    /// Embassy IP stack. Its device runner remains Platform-owned.
    pub ip_stack: Stack<'static>,
    /// Platform Wi-Fi mechanism consumed by the Wi-Fi Plugin.
    pub wifi: Wifi,
    /// Platform entropy source.
    pub entropy: Entropy,
    /// Arbitrarily named regions projected from the Platform's native layout.
    pub partitions: Partitions,
}

/// Result of initializing one statically selected [`Platform`].
pub type PlatformInitResult<P> = Result<
    PlatformResources<<P as Platform>::Partitions, <P as Platform>::Wifi, <P as Platform>::Entropy>,
    <P as Platform>::Error,
>;

/// One statically selected Barracuda execution platform.
///
/// Implementations initialize platform mechanisms and spawn their permanent
/// tasks. Board-specific peripheral construction remains outside this
/// trait in the selected Target composition.
pub trait Platform: Sized + 'static {
    /// Board/HAL-produced inputs required by this Platform implementation.
    ///
    /// Standard Platforms commonly use `&'static Board`. Device Platforms use
    /// a concrete binding type containing only the chip resources that Target
    /// composition assigned to Platform mechanisms.
    type Bindings;
    /// Concrete Wi-Fi mechanism supplied to System composition.
    type Wifi: WifiDevice;
    /// Concrete entropy source supplied to System composition.
    type Entropy: Entropy;
    /// Generic named partition collection produced from the native layout.
    type Partitions;
    /// Platform initialization failure.
    type Error;

    /// Installs process-lifetime runtime support required before initialization.
    fn prepare() -> Result<(), Self::Error> {
        Ok(())
    }

    /// Initializes the Platform for an independently selected Board.
    fn initialize(
        spawner: Spawner,
        bindings: Self::Bindings,
    ) -> impl Future<Output = PlatformInitResult<Self>>;
}
