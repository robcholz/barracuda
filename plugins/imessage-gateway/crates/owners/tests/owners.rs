//! Owner persistence through real Plugin storage.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]

use std::cell::Cell;
use std::rc::Rc;

use barracuda_imessage_gateway_owners::{
    Classification, Owner, Owners, OwnersError, PairingEntropy, OWNERS_STORAGE_KEY,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::memory_partition;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use futures_lite::future::block_on;

#[derive(Clone)]
struct Sequence(Rc<Cell<u32>>);

impl Entropy for Sequence {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = self.0.get();
        self.0.set(value.wrapping_add(1));
        bytes.copy_from_slice(&value.to_le_bytes()[..bytes.len()]);
        Ok(())
    }
}

fn entropy() -> PairingEntropy {
    PairingEntropy::new(Sequence(Rc::new(Cell::new(7))))
}

/// Runs `scenario` against this Plugin's scoped storage during registration.
struct Probe<F>(Option<F>);

impl<F> PluginDeclaration for Probe<F> {
    const ID: &'static str = "owners-probe";
    const DEPENDS_ON: &'static [&'static str] = &[];
}

trait Scenario {
    fn run<Storage: PluginStorage>(self, storage: Storage);
}

impl<F: Scenario + 'static> Plugin for Probe<F> {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        if let Some(scenario) = self.0.take() {
            scenario.run(context.storage().clone());
        }
        Ok(())
    }
}

fn with_storage(scenario: impl Scenario + 'static) {
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager
        .register(Probe(Some(scenario)))
        .expect("register owners probe");
}

struct PairPersistsAcrossReload;

impl Scenario for PairPersistsAcrossReload {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let owners = Owners::load(storage.clone(), entropy())
                .await
                .expect("empty owners");
            assert_eq!(owners.count(), 0);
            assert_eq!(
                owners.classify("42", Some("Ann"), "hello").await,
                Classification::Ignored
            );
            let code = owners.pairing().expect("pairing code").code;
            assert_eq!(code.as_str(), "000007");
            assert_eq!(
                owners.classify("42", Some("Ann"), code.as_str()).await,
                Classification::Paired
            );
            assert_eq!(
                owners.classify("42", None, "hello").await,
                Classification::Owner
            );
            assert_eq!(owners.ignored(), 1);
            drop(owners);

            let reloaded = Owners::load(storage.clone(), entropy())
                .await
                .expect("stored owners");
            assert_eq!(
                reloaded.owners(),
                vec![Owner::new("42", Some("Ann")).expect("owner")]
            );
            assert_eq!(reloaded.ignored(), 0, "the counter is not stored");
            assert!(reloaded.is_owner("42"));

            assert!(reloaded.remove("42").await.expect("remove"));
            assert!(!reloaded.remove("42").await.expect("remove twice"));
            let stored = storage
                .get_bytes(OWNERS_STORAGE_KEY)
                .await
                .expect("read")
                .expect("stored list");
            assert_eq!(stored, b"[]");
        });
    }
}

#[test]
fn paired_owners_persist_and_removal_is_stored() {
    with_storage(PairPersistsAcrossReload);
}

struct InvalidStoredList;

impl Scenario for InvalidStoredList {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            storage
                .put(OWNERS_STORAGE_KEY, b"{not json".as_slice())
                .await
                .expect("write");
            assert!(matches!(
                Owners::load(storage, entropy()).await,
                Err(OwnersError::Invalid)
            ));
        });
    }
}

#[test]
fn an_invalid_stored_list_is_reported() {
    with_storage(InvalidStoredList);
}

struct RotateOnRequest;

impl Scenario for RotateOnRequest {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let owners = Owners::load(storage, entropy()).await.expect("owners");
            let first = owners.pairing().expect("code").code;
            owners.rotate().expect("rotate");
            let second = owners.pairing().expect("code").code;
            assert_ne!(first, second);
            assert_eq!(
                owners.classify("1", None, first.as_str()).await,
                Classification::Ignored
            );
            assert_eq!(
                owners.classify("1", None, second.as_str()).await,
                Classification::Paired
            );
        });
    }
}

#[test]
fn rotate_retires_the_shown_code() {
    with_storage(RotateOnRequest);
}
