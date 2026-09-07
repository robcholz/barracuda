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
fn write_transaction_accepts_variable_length_utf8_values() {
    block_on(async {
        let database = database().await;
        let mut write = database.write_transaction().await;
        write
            .write("api_base", "https://api.tavily.com")
            .await
            .expect("write API base");
        write
            .write("api_key", "secret")
            .await
            .expect("write API key");
        write.commit().await.expect("commit strings");

        let read = database.read_transaction().await;
        let mut entries = read
            .entries_in_range("api_", "api`")
            .await
            .expect("open string entries");
        let base = entries.next().await.expect("read base").expect("base");
        assert_eq!(base.key(), "api_base");
        assert_eq!(base.value_bytes(), b"https://api.tavily.com");
        let key = entries.next().await.expect("read key").expect("key");
        assert_eq!(key.key(), "api_key");
        assert_eq!(key.value_bytes(), b"secret");
    });
}

#[test]
fn read_transaction_returns_owned_variable_length_bytes() {
    block_on(async {
        let database = database().await;
        let mut write = database.write_transaction().await;
        write
            .write("configuration", br#"{"token":"secret"}"#.as_slice())
            .await
            .expect("write configuration");
        write.commit().await.expect("commit configuration");

        let read = database.read_transaction().await;
        assert_eq!(
            read.read_bytes("configuration")
                .await
                .expect("read configuration"),
            br#"{"token":"secret"}"#
        );
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
fn read_entry_iterator_streams_values_inside_the_requested_range() {
    block_on(async {
        let database = database().await;
        let mut write = database.write_transaction().await;
        write.write("before", &0_u32).await.expect("write before");
        write
            .write("scope:alpha", &1_u32)
            .await
            .expect("write alpha");
        write.write("scope:beta", &2_u32).await.expect("write beta");
        write
            .write("scope;after", &3_u32)
            .await
            .expect("write after");
        write.commit().await.expect("commit entries");

        let read = database.read_transaction().await;
        let mut entries = read
            .entries_in_range("scope:", "scope;")
            .await
            .expect("open range iterator");

        let alpha = entries.next().await.expect("read alpha").expect("alpha");
        assert_eq!(alpha.key(), "scope:alpha");
        assert_eq!(alpha.value::<u32>().expect("decode alpha"), 1);

        let beta = entries.next().await.expect("read beta").expect("beta");
        assert_eq!(beta.key(), "scope:beta");
        assert_eq!(beta.value::<u32>().expect("decode beta"), 2);
        assert_eq!(entries.next().await.expect("finish range"), None);
    });
}
