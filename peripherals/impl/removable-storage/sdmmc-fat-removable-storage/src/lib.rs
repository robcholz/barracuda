//! FAT removable-filesystem implementation over a Board-selected SDMMC device.

#![no_std]

extern crate alloc;

use core::marker::PhantomData;

use barracuda_peripheral::PeripheralImplementation;
use barracuda_peripheral::removable_storage::{
    RemovableStorage, RemovableStorageEvent, RemovableStorageStatus,
};
use barracuda_vfs_fat::FatFs;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_io::{ErrorKind, ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use portable_atomic_util::Arc;

const RETRY_INTERVAL_MILLIS: u64 = 1_000;
const PRESENCE_POLL_MILLIS: u64 = 500;

/// Move-only SDMMC transport consumed by this implementation.
pub struct SdMmcFatRemovableStorageBindings<DEVICE> {
    device: DEVICE,
}

impl<DEVICE> SdMmcFatRemovableStorageBindings<DEVICE> {
    /// Wraps the Platform SDMMC data plane.
    #[must_use]
    pub const fn new(device: DEVICE) -> Self {
        Self { device }
    }
}

/// Stable Board slot identity used to construct the VFS mount path.
pub struct SdMmcFatRemovableStorageConfig {
    slot_id: &'static str,
}

impl SdMmcFatRemovableStorageConfig {
    /// Creates configuration for one generated Board peripheral instance.
    #[must_use]
    pub const fn new(slot_id: &'static str) -> Self {
        Self { slot_id }
    }
}

struct SharedDevice<DEVICE> {
    device: Mutex<CriticalSectionRawMutex, DEVICE>,
    generation: AtomicU32,
    connected: AtomicBool,
}

impl<DEVICE> SharedDevice<DEVICE> {
    fn new(device: DEVICE) -> Self {
        Self {
            device: Mutex::new(device),
            generation: AtomicU32::new(0),
            connected: AtomicBool::new(false),
        }
    }

    fn begin_mount(this: &Arc<Self>) -> SessionDevice<DEVICE> {
        let generation = this
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        this.connected.store(true, Ordering::Release);
        SessionDevice::new(Arc::clone(this), generation)
    }

    fn probe(this: &Arc<Self>, generation: u32) -> SessionDevice<DEVICE> {
        SessionDevice::new(Arc::clone(this), generation)
    }

    fn is_connected(&self, generation: u32) -> bool {
        self.connected.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }

    fn invalidate(&self, generation: u32) {
        if self
            .generation
            .compare_exchange(
                generation,
                generation.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.connected.store(false, Ordering::Release);
        }
    }
}

#[derive(Debug)]
enum SessionError<DeviceError> {
    MediaRemoved,
    InvalidSeek,
    Device(DeviceError),
}

impl<DeviceError: embedded_io::Error> core::fmt::Display for SessionError<DeviceError> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MediaRemoved => formatter.write_str("the removable medium is no longer present"),
            Self::InvalidSeek => formatter.write_str("the requested seek position is invalid"),
            Self::Device(error) => write!(formatter, "removable device error: {error}"),
        }
    }
}

impl<DeviceError: embedded_io::Error> core::error::Error for SessionError<DeviceError> {}

impl<DeviceError: embedded_io::Error> embedded_io::Error for SessionError<DeviceError> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::MediaRemoved => ErrorKind::NotConnected,
            Self::InvalidSeek => ErrorKind::InvalidInput,
            Self::Device(error) => error.kind(),
        }
    }
}

struct SessionDevice<DEVICE> {
    shared: Arc<SharedDevice<DEVICE>>,
    generation: u32,
    position: u64,
}

impl<DEVICE> SessionDevice<DEVICE> {
    fn new(shared: Arc<SharedDevice<DEVICE>>, generation: u32) -> Self {
        Self {
            shared,
            generation,
            position: 0,
        }
    }

    fn ensure_current<ERROR>(&self) -> Result<(), SessionError<ERROR>> {
        if self.shared.is_connected(self.generation) {
            Ok(())
        } else {
            Err(SessionError::MediaRemoved)
        }
    }

    fn device_error<ERROR: embedded_io::Error>(&self, error: ERROR) -> SessionError<ERROR> {
        if error.kind() != ErrorKind::InvalidInput {
            self.shared.invalidate(self.generation);
        }
        SessionError::Device(error)
    }
}

impl<DEVICE: ErrorType> ErrorType for SessionDevice<DEVICE> {
    type Error = SessionError<DEVICE::Error>;
}

impl<DEVICE> Read for SessionDevice<DEVICE>
where
    DEVICE: Read + Seek + Send,
    DEVICE::Error: embedded_io::Error,
{
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.ensure_current()?;
        let mut device = self.shared.device.lock().await;
        self.ensure_current()?;
        device
            .seek(SeekFrom::Start(self.position))
            .await
            .map_err(|error| self.device_error(error))?;
        let read = device
            .read(buffer)
            .await
            .map_err(|error| self.device_error(error))?;
        self.position = self.position.saturating_add(read as u64);
        Ok(read)
    }
}

impl<DEVICE> Write for SessionDevice<DEVICE>
where
    DEVICE: Write + Seek + Send,
    DEVICE::Error: embedded_io::Error,
{
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.ensure_current()?;
        let mut device = self.shared.device.lock().await;
        self.ensure_current()?;
        device
            .seek(SeekFrom::Start(self.position))
            .await
            .map_err(|error| self.device_error(error))?;
        let written = device
            .write(buffer)
            .await
            .map_err(|error| self.device_error(error))?;
        self.position = self.position.saturating_add(written as u64);
        Ok(written)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.ensure_current()?;
        let mut device = self.shared.device.lock().await;
        self.ensure_current()?;
        device
            .flush()
            .await
            .map_err(|error| self.device_error(error))
    }
}

impl<DEVICE> Seek for SessionDevice<DEVICE>
where
    DEVICE: Seek + Send,
    DEVICE::Error: embedded_io::Error,
{
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.ensure_current()?;
        let next = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(offset) => seek_offset(self.position, offset)?,
            SeekFrom::End(offset) => {
                let mut device = self.shared.device.lock().await;
                self.ensure_current()?;
                device
                    .seek(SeekFrom::End(offset))
                    .await
                    .map_err(|error| self.device_error(error))?
            }
        };
        self.position = next;
        Ok(next)
    }
}

fn seek_offset<ERROR>(position: u64, offset: i64) -> Result<u64, SessionError<ERROR>> {
    let next = i128::from(position)
        .checked_add(i128::from(offset))
        .ok_or(SessionError::InvalidSeek)?;
    u64::try_from(next).map_err(|_| SessionError::InvalidSeek)
}

enum SlotState {
    Searching { first_attempt: bool },
    Mounted { generation: u32 },
}

/// Board-attached SD slot that reports mountable FAT filesystem generations.
pub struct SdMmcFatRemovableStorage<DEVICE> {
    slot_id: &'static str,
    shared: Arc<SharedDevice<DEVICE>>,
    state: SlotState,
}

impl<DEVICE> RemovableStorage for SdMmcFatRemovableStorage<DEVICE>
where
    DEVICE: ErrorType + Read + Write + Seek + Send + 'static,
    DEVICE::Error: embedded_io::Error + Send,
{
    fn slot_id(&self) -> &'static str {
        self.slot_id
    }

    fn status(&self) -> RemovableStorageStatus {
        match self.state {
            SlotState::Searching { .. } => RemovableStorageStatus::Absent,
            SlotState::Mounted { generation } => RemovableStorageStatus::Mounted { generation },
        }
    }

    async fn next_event(&mut self) -> RemovableStorageEvent {
        match self.state {
            SlotState::Searching { first_attempt } => {
                let mut immediate = first_attempt;
                loop {
                    if !immediate {
                        embassy_time::Timer::after_millis(RETRY_INTERVAL_MILLIS).await;
                    }
                    immediate = false;
                    let session = SharedDevice::begin_mount(&self.shared);
                    let generation = session.generation;
                    if let Ok(filesystem) = FatFs::mount(session).await {
                        self.state = SlotState::Mounted { generation };
                        return RemovableStorageEvent::Mounted {
                            generation,
                            filesystem: filesystem.into_backend(),
                        };
                    }
                }
            }
            SlotState::Mounted { generation } => loop {
                embassy_time::Timer::after_millis(PRESENCE_POLL_MILLIS).await;
                let mut probe = SharedDevice::probe(&self.shared, generation);
                let mut byte = [0_u8; 1];
                let present = probe.seek(SeekFrom::Start(0)).await.is_ok()
                    && matches!(probe.read(&mut byte).await, Ok(1));
                if !present || !self.shared.is_connected(generation) {
                    self.shared.invalidate(generation);
                    self.state = SlotState::Searching {
                        first_attempt: false,
                    };
                    return RemovableStorageEvent::Removed { generation };
                }
            },
        }
    }
}

/// Static factory used by generated Board composition.
pub struct SdMmcFatRemovableStorageImplementation<DEVICE>(PhantomData<DEVICE>);

impl<DEVICE> PeripheralImplementation for SdMmcFatRemovableStorageImplementation<DEVICE>
where
    DEVICE: ErrorType + Read + Write + Seek + Send + 'static,
    DEVICE::Error: embedded_io::Error + Send,
{
    type Bindings = SdMmcFatRemovableStorageBindings<DEVICE>;
    type Config = SdMmcFatRemovableStorageConfig;
    type Peripheral = SdMmcFatRemovableStorage<DEVICE>;
    type Error = core::convert::Infallible;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ok(SdMmcFatRemovableStorage {
            slot_id: config.slot_id,
            shared: Arc::new(SharedDevice::new(bindings.device)),
            state: SlotState::Searching {
                first_attempt: true,
            },
        })
    }
}
