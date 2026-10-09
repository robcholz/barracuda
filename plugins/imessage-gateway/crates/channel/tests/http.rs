//! Shared channel endpoints and status over a mock channel and real Plugin
//! storage.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]

use std::cell::Cell;
use std::rc::Rc;

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};
use barracuda_imessage_gateway_channel::{
    entry_status, load_mode, receive_runtime, status_response, store_mode, sync_receive,
    ChannelControl, ChannelMode, Classification, ModeEndpoint, ModeError, ModeFuture, Owners,
    OwnersEndpoint, PairingEntropy, ReceiveChannel, ReceiveControl, ReceiveError, ReceiveFuture,
    ReceiveSession, ReceiveSlotSource, ReceiveState, ReceiveTiming, MODE_STORAGE_KEY,
};
use barracuda_platform::{Entropy, EntropyUnavailable};
use barracuda_platform_test::memory_partition;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
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

/// One-slot pool; `taken` marks it held by another channel.
struct Pool {
    taken: Rc<Cell<bool>>,
}

struct Lease(Rc<Cell<bool>>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl ReceiveSlotSource for Pool {
    type Lease = Lease;

    fn acquire(&self) -> Option<Lease> {
        (!self.taken.replace(true)).then(|| Lease(Rc::clone(&self.taken)))
    }

    fn capacity(&self) -> usize {
        1
    }
}

struct Mock<Storage> {
    storage: Storage,
    configured: Cell<bool>,
    mode: Cell<ChannelMode>,
    modes: &'static [ChannelMode],
    failure: Cell<Option<ModeError>>,
    receive: Rc<ReceiveControl<Pool>>,
    owners: Owners<Storage>,
}

impl<Storage: PluginStorage> ChannelControl for Mock<Storage> {
    type Storage = Storage;
    type Slots = Pool;

    fn configured(&self) -> bool {
        self.configured.get()
    }

    fn mode(&self) -> ChannelMode {
        self.mode.get()
    }

    fn modes(&self) -> &'static [ChannelMode] {
        self.modes
    }

    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_> {
        Box::pin(async move {
            if let Some(failure) = self.failure.get() {
                return Err(failure);
            }
            store_mode(&self.storage, mode)
                .await
                .map_err(|_error| ModeError::Storage)?;
            self.mode.set(mode);
            Ok(())
        })
    }

    fn receive(&self) -> &ReceiveControl<Pool> {
        &self.receive
    }

    fn owners(&self) -> &Owners<Storage> {
        &self.owners
    }
}

/// Fails every session with "closed".
struct Closed;

impl ReceiveChannel<Lease> for Closed {
    fn receive<'a>(
        &'a self,
        _lease: &'a mut Lease,
        _session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        Box::pin(async { Err(ReceiveError::new("closed")) })
    }
}

struct Harness<Storage> {
    channel: Rc<Mock<Storage>>,
    mode: ModeEndpoint<Mock<Storage>>,
    owners: OwnersEndpoint<Mock<Storage>>,
    taken: Rc<Cell<bool>>,
}

impl<Storage: PluginStorage> Harness<Storage> {
    async fn new(storage: Storage, entropy: PairingEntropy, modes: &'static [ChannelMode]) -> Self {
        let taken = Rc::new(Cell::new(false));
        let channel = Rc::new(Mock {
            owners: Owners::load(storage.clone(), entropy)
                .await
                .expect("owners"),
            storage,
            configured: Cell::new(true),
            mode: Cell::new(ChannelMode::Send),
            modes,
            failure: Cell::new(None),
            receive: Rc::new(ReceiveControl::new(Pool {
                taken: Rc::clone(&taken),
            })),
        });
        Self {
            mode: ModeEndpoint::new(Rc::clone(&channel)),
            owners: OwnersEndpoint::new(Rc::clone(&channel)),
            channel,
            taken,
        }
    }

    fn status(&self) -> String {
        body(status_response(self.channel.as_ref()))
    }

    async fn post_mode(&self, body: &str) -> (u16, String) {
        call(&self.mode, HttpMethod::Post, body).await
    }

    async fn owners(&self, method: HttpMethod, body: &str) -> (u16, String) {
        call(&self.owners, method, body).await
    }

    async fn stored_mode(&self) -> String {
        let bytes = self
            .channel
            .storage
            .get_bytes(MODE_STORAGE_KEY)
            .await
            .expect("read")
            .expect("stored mode");
        String::from_utf8(bytes).expect("UTF-8")
    }
}

fn body(response: barracuda_webserver_plugin::HttpResponse) -> String {
    assert_eq!(response.content_type(), "application/json");
    String::from_utf8(response.body().expect("buffered").to_vec()).expect("UTF-8")
}

async fn call(endpoint: &dyn HttpEndpoint, method: HttpMethod, body: &str) -> (u16, String) {
    let response = endpoint
        .handle(HttpRequest::new(method, body.as_bytes().to_vec()))
        .await;
    let status = response.status();
    let body = String::from_utf8(response.body().expect("buffered").to_vec()).expect("UTF-8");
    (status, body)
}

fn entropy() -> PairingEntropy {
    PairingEntropy::new(Sequence(Rc::new(Cell::new(7))))
}

trait Scenario {
    fn run<Storage: PluginStorage>(self, storage: Storage);
}

struct Probe<S>(Option<S>);

impl<S> PluginDeclaration for Probe<S> {
    const ID: &'static str = "channel-probe";
    const DEPENDS_ON: &'static [&'static str] = &[];
}

impl<S: Scenario + 'static> Plugin for Probe<S> {
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
        .expect("register channel probe");
}

struct StatusAndMode;

impl Scenario for StatusAndMode {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let harness = Harness::new(storage, entropy(), ChannelMode::ALL).await;
            assert_eq!(
                harness.status(),
                r#"{"configured":true,"mode":"send","owners":{"count":0}}"#
            );

            assert_eq!(
                harness.post_mode(r#"{"mode":"send_receive"}"#).await,
                (204, String::new())
            );
            assert_eq!(harness.stored_mode().await, r#""send_receive""#);
            assert!(harness.taken.get(), "the slot is taken at once");
            assert_eq!(
                harness.status(),
                r#"{"configured":true,"mode":"send_receive","receive":{"state":"starting"},"owners":{"count":0}}"#
            );

            assert_eq!(
                harness.post_mode(r#"{"mode":"disabled"}"#).await,
                (204, String::new())
            );
            assert!(!harness.taken.get(), "the slot is freed");
            assert_eq!(
                harness.status(),
                r#"{"configured":true,"mode":"disabled","owners":{"count":0}}"#
            );

            // Every slot in use: the mode is saved, receive reports no_slot.
            harness.taken.set(true);
            assert_eq!(
                harness.post_mode(r#"{"mode":"send_receive"}"#).await,
                (409, r#"{"error":"no_slot","capacity":1}"#.into())
            );
            assert_eq!(harness.channel.mode(), ChannelMode::SendReceive);
            assert_eq!(harness.stored_mode().await, r#""send_receive""#);
            assert_eq!(
                harness.status(),
                r#"{"configured":true,"mode":"send_receive","receive":{"state":"no_slot","capacity":1},"owners":{"count":0}}"#
            );
            assert_eq!(
                entry_status(harness.channel.as_ref()),
                EntryStatus::new(
                    EntryState::Attention,
                    WebText {
                        zh: "名额已满",
                        en: "No slot"
                    }
                )
            );

            // An unconfigured channel saves the mode without receiving.
            harness.taken.set(false);
            assert_eq!(harness.post_mode(r#"{"mode":"send"}"#).await.0, 204);
            harness.channel.configured.set(false);
            assert_eq!(harness.post_mode(r#"{"mode":"send_receive"}"#).await.0, 204);
            assert!(!harness.taken.get());
            assert_eq!(
                harness.status(),
                r#"{"configured":false,"mode":"send_receive","receive":{"state":"idle"},"owners":{"count":0}}"#
            );
            assert_eq!(
                entry_status(harness.channel.as_ref()),
                EntryStatus::configured(false)
            );
        });
    }
}

#[test]
fn status_and_mode_endpoints_report_exact_json() {
    with_storage(StatusAndMode);
}

struct ModeErrors;

impl Scenario for ModeErrors {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let harness = Harness::new(storage, entropy(), ChannelMode::WITHOUT_SEND_ONLY).await;
            for body in [
                "",
                "{}",
                r#"{"mode":"receive"}"#,
                r#"{"mode":"send","extra":1}"#,
            ] {
                assert_eq!(
                    harness.post_mode(body).await,
                    (400, r#"{"error":"invalid_request"}"#.into()),
                    "{body}"
                );
            }
            assert_eq!(
                harness.post_mode(r#"{"mode":"send"}"#).await,
                (400, r#"{"error":"unsupported_mode"}"#.into())
            );
            harness.channel.failure.set(Some(ModeError::Storage));
            assert_eq!(
                harness.post_mode(r#"{"mode":"disabled"}"#).await,
                (500, r#"{"error":"storage"}"#.into())
            );
            harness.channel.failure.set(Some(ModeError::Registration));
            assert_eq!(
                harness.post_mode(r#"{"mode":"send_receive"}"#).await,
                (422, r#"{"error":"registration_failed"}"#.into())
            );
            assert!(!harness.taken.get(), "a failed mode change takes no slot");
            assert_eq!(
                call(&harness.mode, HttpMethod::Get, "").await,
                (405, r#"{"error":"method_not_allowed"}"#.into())
            );
        });
    }
}

#[test]
fn mode_endpoint_rejects_bad_requests() {
    with_storage(ModeErrors);
}

struct ErrorState;

impl Scenario for ErrorState {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let harness = Harness::new(storage, entropy(), ChannelMode::ALL).await;
            let runtime = receive_runtime(
                Rc::clone(&harness.channel.receive),
                Rc::new(Closed),
                ReceiveTiming::DEVICE,
            );
            let test = async {
                assert_eq!(harness.post_mode(r#"{"mode":"send_receive"}"#).await.0, 204);
                let deadline = Instant::now() + Duration::from_secs(2);
                while harness.channel.receive().state().name() != "error" {
                    assert!(Instant::now() < deadline, "no error state");
                    Timer::after_millis(1).await;
                }
                assert_eq!(
                    harness.status(),
                    r#"{"configured":true,"mode":"send_receive","receive":{"state":"error","message":"closed"},"owners":{"count":0}}"#
                );
                assert_eq!(
                    entry_status(harness.channel.as_ref()),
                    EntryStatus::new(
                        EntryState::Attention,
                        WebText {
                            zh: "连接中断",
                            en: "Disconnected"
                        }
                    )
                );
            };
            match select(runtime, test).await {
                Either::First(()) => panic!("runtime ended"),
                Either::Second(()) => {}
            }
        });
    }
}

#[test]
fn status_reports_receive_errors() {
    with_storage(ErrorState);
}

struct OwnersFlow;

impl Scenario for OwnersFlow {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let harness = Harness::new(storage.clone(), entropy(), ChannelMode::ALL).await;
            assert_eq!(
                harness.owners(HttpMethod::Get, "").await,
                (
                    200,
                    r#"{"owners":[],"pairing":{"code":"000007","expires_in":600},"ignored":0}"#
                        .into()
                )
            );
            let owners = harness.channel.owners();
            assert_eq!(
                owners.classify("9", None, "hi").await,
                Classification::Ignored
            );
            assert_eq!(
                owners.classify("42", Some("Ann"), "000007").await,
                Classification::Paired
            );
            assert_eq!(
                harness.owners(HttpMethod::Get, "").await,
                (
                    200,
                    r#"{"owners":[{"id":"42","label":"Ann"}],"pairing":{"code":"000008","expires_in":600},"ignored":1}"#
                        .into()
                )
            );
            assert_eq!(
                harness.status(),
                r#"{"configured":true,"mode":"send","owners":{"count":1}}"#
            );

            assert_eq!(
                harness.owners(HttpMethod::Post, r#"{"rotate":true}"#).await,
                (204, String::new())
            );
            let (_, listed) = harness.owners(HttpMethod::Get, "").await;
            assert!(listed.contains(r#""code":"000009""#), "{listed}");

            assert_eq!(
                owners.classify("7", None, "/start 000009").await,
                Classification::Paired
            );
            assert_eq!(
                harness.owners(HttpMethod::Post, r#"{"remove":"42"}"#).await,
                (204, String::new())
            );
            assert_eq!(
                harness
                    .owners(HttpMethod::Post, r#"{"remove":"unknown"}"#)
                    .await,
                (204, String::new())
            );
            let (_, listed) = harness.owners(HttpMethod::Get, "").await;
            assert!(
                listed.starts_with(
                    r#"{"owners":[{"id":"7","label":null}],"pairing":{"code":"000010""#
                ),
                "{listed}"
            );
            let reloaded = Owners::load(storage, entropy())
                .await
                .expect("stored owners");
            assert!(reloaded.is_owner("7") && !reloaded.is_owner("42"));

            for body in [
                "",
                "{}",
                r#"{"rotate":false}"#,
                r#"{"remove":"7","rotate":true}"#,
                r#"{"remove":7}"#,
            ] {
                assert_eq!(
                    harness.owners(HttpMethod::Post, body).await,
                    (400, r#"{"error":"invalid_request"}"#.into()),
                    "{body}"
                );
            }
            assert_eq!(
                harness.owners(HttpMethod::Delete, "").await,
                (405, r#"{"error":"method_not_allowed"}"#.into())
            );
        });
    }
}

#[test]
fn owners_endpoint_lists_pairs_rotates_and_removes() {
    with_storage(OwnersFlow);
}

struct OwnersWithoutEntropy;

impl Scenario for OwnersWithoutEntropy {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            let harness =
                Harness::new(storage, PairingEntropy::unavailable(), ChannelMode::ALL).await;
            assert_eq!(
                harness.owners(HttpMethod::Get, "").await,
                (200, r#"{"owners":[],"pairing":null,"ignored":0}"#.into())
            );
            assert_eq!(
                harness.owners(HttpMethod::Post, r#"{"rotate":true}"#).await,
                (503, r#"{"error":"entropy_unavailable"}"#.into())
            );
        });
    }
}

#[test]
fn owners_endpoint_without_entropy_offers_no_code() {
    with_storage(OwnersWithoutEntropy);
}

struct ModeMigration;

impl Scenario for ModeMigration {
    fn run<Storage: PluginStorage>(self, storage: Storage) {
        block_on(async {
            // A channel configured before modes existed keeps sending only.
            assert_eq!(
                load_mode(&storage, true, ChannelMode::legacy())
                    .await
                    .expect("mode"),
                ChannelMode::Send
            );
            // The migrated mode is stored, so it no longer depends on `configured`.
            assert_eq!(
                load_mode(&storage, false, ChannelMode::legacy())
                    .await
                    .expect("mode"),
                ChannelMode::Send
            );
            storage.delete(MODE_STORAGE_KEY).await.expect("delete");
            // WeChat migrates to send_receive.
            assert_eq!(
                load_mode(&storage, true, ChannelMode::SendReceive)
                    .await
                    .expect("mode"),
                ChannelMode::SendReceive
            );
            storage.delete(MODE_STORAGE_KEY).await.expect("delete");
            // A channel without a configuration gets the default.
            assert_eq!(
                load_mode(&storage, false, ChannelMode::legacy())
                    .await
                    .expect("mode"),
                ChannelMode::SendReceive
            );
            store_mode(&storage, ChannelMode::Disabled)
                .await
                .expect("store");
            assert_eq!(
                load_mode(&storage, true, ChannelMode::legacy())
                    .await
                    .expect("mode"),
                ChannelMode::Disabled
            );
            storage
                .put(MODE_STORAGE_KEY, b"\"bogus\"".as_slice())
                .await
                .expect("put");
            assert_eq!(
                load_mode(&storage, true, ChannelMode::legacy())
                    .await
                    .expect("mode"),
                ChannelMode::Send
            );
        });
    }
}

#[test]
fn load_mode_migrates_old_and_new_channels() {
    with_storage(ModeMigration);
}

#[test]
fn sync_receive_follows_configuration_and_mode() {
    struct Sync;
    impl Scenario for Sync {
        fn run<Storage: PluginStorage>(self, storage: Storage) {
            block_on(async {
                let harness = Harness::new(storage, entropy(), ChannelMode::ALL).await;
                harness.channel.mode.set(ChannelMode::SendReceive);
                assert_eq!(sync_receive(harness.channel.as_ref()), Ok(()));
                assert!(harness.channel.receive().is_enabled());
                harness.channel.configured.set(false);
                assert_eq!(sync_receive(harness.channel.as_ref()), Ok(()));
                assert!(!harness.channel.receive().is_enabled());
                assert_eq!(harness.channel.receive().state(), ReceiveState::Idle);
            });
        }
    }
    with_storage(Sync);
}
