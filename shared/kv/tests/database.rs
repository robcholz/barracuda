//! Public database behavior over Board-owned partitions.

#![allow(clippy::expect_used)]

use barracuda_kv::{Database, Error, MAX_CAPACITY};
use barracuda_platform_test::{memory_partition, MemoryPartition};
use futures_lite::future::block_on;
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

#[repr(C)]
#[derive(Clone, Copy, Debug, Immutable, IntoBytes, KnownLayout, PartialEq, Eq, TryFromBytes)]
struct Record {
    value: u32,
    generation: u32,
}

async fn database() -> Database<MemoryPartition> {
    let partition = memory_partition(MAX_CAPACITY)
        .await
        .expect("create test database region");
    Database::open(partition).await.expect("open database")
}

#[test]
fn committed_write_transaction_persists_every_write() {
    block_on(async {
        let database = database().await;
        let mut write = database.write_transaction().await;
        let count = Record {
            value: 1,
            generation: 3,
        };
        let state = Record {
            value: 7,
            generation: 4,
        };
        write.write("count", &count).await.expect("write count");
        write.write("state", &state).await.expect("write state");
        write.commit().await.expect("commit transaction");

        let read = database.read_transaction().await;
        assert_eq!(
            read.read::<Record>("state").await.expect("read state"),
            state
        );
        assert_eq!(
            read.read::<Record>("count").await.expect("read count"),
            count
        );
        assert_eq!(read.read::<u32>("count").await, Err(Error::InvalidValue));
    });
}

#[test]
fn dropping_write_transaction_rolls_back_every_write() {
    block_on(async {
        let database = database().await;
        let mut write = database.write_transaction().await;
        let first = Record {
            value: 1,
            generation: 1,
        };
        let second = Record {
            value: 2,
            generation: 1,
        };
        write.write("first", &first).await.expect("stage first");
        write.write("second", &second).await.expect("stage second");
        drop(write);

        let read = database.read_transaction().await;
        assert_eq!(read.read::<Record>("first").await, Err(Error::KeyNotFound));
        assert_eq!(read.read::<Record>("second").await, Err(Error::KeyNotFound));
    });
}
