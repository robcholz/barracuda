//! Public database behavior over Board-owned partitions.

#![allow(clippy::expect_used)]

use barracuda_kv::{
    Database, Error, FlashGeometryError, OpenError, MAX_CAPACITY, MAX_KEY_SIZE, MAX_VALUE_SIZE,
    PAGE_SIZE,
};
use barracuda_platform_test::{memory_partition, MemoryNorFlash, MemoryPartition};
use embedded_storage_async::nor_flash::NorFlash as _;
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

#[test]
fn replacements_deletes_and_transaction_ordering_are_atomic() {
    block_on(async {
        let database = database().await;
        let original = Record {
            value: 10,
            generation: 1,
        };
        let replacement = Record {
            value: 11,
            generation: 2,
        };
        let mut seed = database.write_transaction().await;
        seed.write("record", &original).await.expect("seed record");
        seed.commit().await.expect("commit seed");

        let mut replace = database.write_transaction().await;
        replace
            .write("record", &replacement)
            .await
            .expect("replace record");
        replace.commit().await.expect("commit replacement");
        assert_eq!(
            database
                .read_transaction()
                .await
                .read::<Record>("record")
                .await,
            Ok(replacement)
        );

        let mut delete = database.write_transaction().await;
        delete
            .delete("missing")
            .await
            .expect("missing delete is benign");
        delete.delete("record").await.expect("delete record");
        delete.commit().await.expect("commit deletion");
        assert_eq!(
            database
                .read_transaction()
                .await
                .read::<Record>("record")
                .await,
            Err(Error::KeyNotFound)
        );

        let mut unordered = database.write_transaction().await;
        unordered.write("z", &1_u32).await.expect("first key");
        assert_eq!(
            unordered.write("a", &2_u32).await,
            Err(Error::KeysNotSorted)
        );
        assert_eq!(unordered.commit().await, Err(Error::TransactionCanceled));
        let read = database.read_transaction().await;
        assert_eq!(read.read::<u32>("z").await, Err(Error::KeyNotFound));
        assert_eq!(read.read::<u32>("a").await, Err(Error::KeyNotFound));
    });
}

#[test]
fn key_value_limits_and_corrupt_existing_storage_are_reported() {
    block_on(async {
        let database = database().await;
        let long_key = "k".repeat(MAX_KEY_SIZE + 1);
        assert_eq!(
            database
                .read_transaction()
                .await
                .read::<u32>(&long_key)
                .await,
            Err(Error::KeyTooLong)
        );
        let mut write = database.write_transaction().await;
        assert_eq!(write.write(&long_key, &1_u32).await, Err(Error::KeyTooLong));
        let oversized = [0_u8; MAX_VALUE_SIZE + 1];
        assert_eq!(
            write.write("large", &oversized).await,
            Err(Error::ValueTooLong)
        );

        let mut corrupt = MemoryNorFlash::new(MAX_CAPACITY);
        corrupt
            .write(0, &[0])
            .await
            .expect("mark storage non-erased");
        assert!(matches!(
            Database::open(corrupt).await,
            Err(OpenError::Database(Error::Corrupted))
        ));
    });
}

#[test]
fn database_rejects_partitions_outside_its_addressable_geometry() {
    block_on(async {
        assert!(matches!(
            Database::open(MemoryNorFlash::new(PAGE_SIZE)).await,
            Err(OpenError::Geometry(FlashGeometryError::PageCount {
                actual: 1,
                ..
            }))
        ));
        assert!(matches!(
            Database::open(MemoryNorFlash::new(PAGE_SIZE + 1)).await,
            Err(OpenError::Geometry(
                FlashGeometryError::CapacityAlignment { .. }
            ))
        ));
        assert!(matches!(
            Database::open(MemoryNorFlash::new(MAX_CAPACITY + PAGE_SIZE)).await,
            Err(OpenError::Geometry(FlashGeometryError::PageCount { .. }))
        ));
    });
}
