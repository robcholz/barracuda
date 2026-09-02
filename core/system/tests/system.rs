#![allow(clippy::expect_used, missing_docs)]

use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use barracuda_board_hal::{BoardHalResources, NoBuiltinCapabilities, NoExposedIo};
use barracuda_event_router::RpcLaneStorage;
use barracuda_platform::{NamedPartition, PartitionAccess, Partitions, PlatformResources};
use barracuda_platform_test::never_embassy_stack;
use barracuda_system::System;
use barracuda_target_api::TargetResources;
use barracuda_tls::PlaintextTls;
use embassy_executor::{Executor, Spawner};
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

struct MemoryFlash {
    bytes: Vec<u8>,
}

impl MemoryFlash {
    fn new(blocks: usize) -> Self {
        Self {
            bytes: vec![0xff; blocks * Self::ERASE_SIZE],
        }
    }

    fn range(&self, offset: u32, length: usize) -> Result<std::ops::Range<usize>, FlashError> {
        let start = usize::try_from(offset).map_err(|_| FlashError)?;
        let end = start.checked_add(length).ok_or(FlashError)?;
        (end <= self.bytes.len())
            .then_some(start..end)
            .ok_or(FlashError)
    }
}

#[derive(Clone, Copy, Debug)]
struct FlashError;

impl NorFlashError for FlashError {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::OutOfBounds
    }
}

impl ErrorType for MemoryFlash {
    type Error = FlashError;
}

impl ReadNorFlash for MemoryFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let source = self
            .bytes
            .get(self.range(offset, bytes.len())?)
            .ok_or(FlashError)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for MemoryFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length =
            usize::try_from(to.checked_sub(from).ok_or(FlashError)?).map_err(|_| FlashError)?;
        let range = self.range(from, length)?;
        self.bytes.get_mut(range).ok_or(FlashError)?.fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len())?;
        let target = self.bytes.get_mut(range).ok_or(FlashError)?;
        for (target, source) in target.iter_mut().zip(bytes) {
            *target &= *source;
        }
        Ok(())
    }
}

fn partition(
    name: &'static str,
    access: PartitionAccess,
    blocks: usize,
) -> NamedPartition<MemoryFlash> {
    NamedPartition::new(name, access, MemoryFlash::new(blocks))
}

#[embassy_executor::task]
async fn start_and_stop_system(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let mut partitions = Partitions::<MemoryFlash, 3>::new();
        partitions
            .insert(partition("system", PartitionAccess::ReadWrite, 16))
            .map_err(|error| error.to_string())?;
        partitions
            .insert(partition("kv_database", PartitionAccess::ReadWrite, 16))
            .map_err(|error| error.to_string())?;
        partitions
            .insert(partition("web_assets", PartitionAccess::ReadOnly, 2))
            .map_err(|error| error.to_string())?;
        let resources = TargetResources {
            platform: PlatformResources {
                ip_stack: never_embassy_stack(),
                tls: PlaintextTls,
                partitions,
            },
            board_hal: BoardHalResources::new(NoBuiltinCapabilities, NoExposedIo),
        };
        let lanes = Box::leak(Box::new(RpcLaneStorage::<64, 512, 16>::new()));
        let system = System::new(lanes, resources, spawner)
            .await
            .map_err(|error| error.to_string())?;
        system.shutdown().await.map_err(|error| error.to_string())
    }
    .await;
    let _sent = completed.send(result);
}

#[test]
fn complete_fixed_plugin_graph_starts_and_shuts_down() {
    let (completed, result) = sync_channel(1);
    std::thread::Builder::new()
        .name("system-lifecycle".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(start_and_stop_system(spawner, completed))
                    .expect("spawn System lifecycle test");
            });
        })
        .expect("spawn System executor thread");

    result
        .recv_timeout(Duration::from_secs(15))
        .expect("System lifecycle timed out")
        .expect("System lifecycle failed");
}
