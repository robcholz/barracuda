#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};
use barracuda_imessage_gateway_channel::{
    receive_runtime, ChannelControl, ChannelEndpoint, ChannelMode, ModeEndpoint, OwnersEndpoint,
    ReceiveState, ReceiveTiming, PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, ChannelFuture, IMessageGateway, IMessageGatewayPlugin, MessageChannel,
    MessageTarget, Operation, SendMessageRequest, SendReceipt,
};
use barracuda_platform_test::{
    install_global_memory_vfs, memory_partition, never_embassy_stack, ScriptStep, ScriptedStack,
};
use barracuda_plugin::api::{Entropy, EntropyUnavailable, PluginContext, SharedEntropy};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{HttpEndpoint, HttpMethod, HttpRequest};
use barracuda_workflow_plugin::WorkflowPlugin;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant};
use embedded_io_async::ErrorKind;
use futures_lite::future::{block_on, yield_now};
use http_client::{ClientFactory, ReceiveLease, ReceiveSlots};
use portable_atomic::{AtomicU32, Ordering};
use portable_atomic_util::Arc;
use serde_json::{json, Value};

use super::{
    PollTiming, WechatReceiver, BOT_ID_STORAGE_KEY, CONTEXT_TOKENS_STORAGE_KEY, CURSOR_STORAGE_KEY,
    SESSION_EXPIRED_MESSAGE,
};
use crate::{
    entry_status, login::LoginWatch, ChannelConfiguration, ConfigEndpoint, ConfigRequest,
    CONFIGURATION_STORAGE_KEY, CONFIG_API_PATH,
};

const API_BASE: &str = "http://wechat.test";
const OWNER: &str = "owner@im.wechat";
/// The code minted from the first draw of [`Counter`].
const PAIRING_CODE: &str = "000007";
const STRANGER: &str = "stranger@im.wechat";

const RECEIVE_TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(10),
    max_backoff: Duration::from_millis(40),
    // Long enough that only a freed slot can wake the runtime in time.
    slot_retry: Duration::from_secs(3600),
};

const POLL_TIMING: PollTiming = PollTiming {
    margin: Duration::from_millis(20),
    min_long_poll: Duration::from_millis(1),
    max_long_poll: Duration::from_secs(120),
    idle_cursor_write: Duration::from_secs(3600),
};

/// Test entropy: a counter, so pairing codes and UINs differ per draw.
#[derive(Clone)]
struct Counter(Arc<AtomicU32>);

impl Entropy for Counter {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let value = self.0.fetch_add(1, Ordering::Relaxed).wrapping_add(7);
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = value.to_le_bytes()[index % 4];
        }
        Ok(())
    }
}

/// What the store holds before the channel loads.
#[derive(Default)]
struct Stored {
    configured: bool,
    mode: Option<&'static str>,
    owners: Vec<&'static str>,
    cursor: Option<&'static str>,
    bot_id: Option<&'static str>,
}

impl Stored {
    fn linked() -> Self {
        Self {
            configured: true,
            ..Self::default()
        }
    }

    fn with_owner(mut self, owner: &'static str) -> Self {
        self.owners.push(owner);
        self
    }
}

/// A test body run inside Plugin registration, where scoped storage exists.
trait Scenario: 'static {
    async fn run<S: PluginStorage>(self, harness: Rc<Harness<S>>);
}

struct Probe<Sc> {
    setup: Option<Setup>,
    scenario: Option<Sc>,
}

impl<Sc: Scenario> PluginDeclaration for Probe<Sc> {
    const ID: &'static str = "wechat-receive-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
}

impl<Sc: Scenario> Plugin for Probe<Sc> {
    fn register<S>(&mut self, context: &mut PluginRegisterContext<'_, S>) -> PluginResult<()>
    where
        S: PluginStorage,
    {
        let setup = self.setup.take().expect("one registration");
        let scenario = self.scenario.take().expect("one registration");
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let storage = context.storage().clone();
        block_on(seed(&storage, &setup.stored));
        let receive: &'static ScriptedStack =
            Box::leak(Box::new(ScriptedStack::new(setup.receive)));
        let send: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new(setup.send)));
        let slots = ReceiveSlots::from_connectors([receive.clone()], receive);
        let blocker = setup
            .slot_taken
            .then(|| slots.acquire().expect("free slot"));
        let channel = Rc::new(block_on(ChannelConfiguration::load(
            Rc::clone(&gateway),
            ClientFactory::from_network(send, send),
            storage.clone(),
            slots.clone(),
            SharedEntropy::new(Counter(Arc::new(AtomicU32::new(0)))),
            POLL_TIMING,
        ))?);
        // The first entropy draw mints the code, so it is known in advance. An
        // unlinked channel has no owner book yet.
        if let Some(owners) = channel.owners() {
            let code = owners.pairing().map(|pairing| pairing.code);
            assert_eq!(code.as_ref().map(|code| code.as_str()), Some(PAIRING_CODE));
        }
        let runtime = receive_runtime(
            Rc::clone(&channel.receive),
            Rc::new(WechatReceiver::new(Rc::clone(&channel))),
            RECEIVE_TIMING,
        );
        let _no_slot = barracuda_imessage_gateway_channel::sync_receive(&*channel);
        let harness = Rc::new(Harness {
            channel,
            gateway,
            receive,
            send,
            slots,
            blocker: RefCell::new(blocker),
            storage,
        });
        block_on(async move {
            match select(runtime, scenario.run(harness)).await {
                Either::First(()) => panic!("receive runtime ended"),
                Either::Second(()) => {}
            }
        });
        Ok(())
    }
}

async fn seed<S: PluginStorage>(storage: &S, stored: &Stored) {
    if stored.configured {
        let config = ConfigRequest::from_login("bot-token".into(), API_BASE.into());
        let bytes = serde_json::to_vec(&config).expect("encode configuration");
        storage
            .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
            .await
            .expect("store configuration");
    }
    if let Some(mode) = stored.mode {
        storage
            .put("mode", format!("\"{mode}\"").as_bytes())
            .await
            .expect("store mode");
    }
    if !stored.owners.is_empty() {
        let owners: Vec<Value> = stored
            .owners
            .iter()
            .map(|id| json!({ "id": id, "label": null }))
            .collect();
        storage
            .put("owners", Value::from(owners).to_string().as_bytes())
            .await
            .expect("store owners");
    }
    if let Some(cursor) = stored.cursor {
        storage
            .put(CURSOR_STORAGE_KEY, cursor.as_bytes())
            .await
            .expect("store cursor");
    }
    if let Some(bot_id) = stored.bot_id {
        storage
            .put(BOT_ID_STORAGE_KEY, bot_id.as_bytes())
            .await
            .expect("store bot id");
    }
}

/// A second `wechat` channel, used to probe the Gateway registration.
struct OtherWechatChannel;

impl MessageChannel for OtherWechatChannel {
    fn channel(&self) -> &str {
        "wechat"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
    }
}

type Channel<S> = ChannelConfiguration<S, ScriptedStack, ScriptedStack, ScriptedStack>;

struct Harness<S> {
    channel: Rc<Channel<S>>,
    gateway: Rc<IMessageGateway>,
    receive: &'static ScriptedStack,
    send: &'static ScriptedStack,
    slots: ReceiveSlots<ScriptedStack, ScriptedStack>,
    blocker: RefCell<Option<ReceiveLease<ScriptedStack, ScriptedStack>>>,
    storage: S,
}

impl<S: PluginStorage> Harness<S> {
    async fn until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; state {:?}, polls {:?}",
                self.channel.receive.state(),
                self.receive_bodies()
            );
            embassy_time::Timer::after(Duration::from_millis(1)).await;
        }
    }

    async fn settle(&self) {
        for _ in 0..200 {
            yield_now().await;
        }
        embassy_time::Timer::after(Duration::from_millis(20)).await;
    }

    fn published(&self) -> Vec<(String, String, String)> {
        self.channel
            .inbound
            .published
            .borrow()
            .iter()
            .map(|message| {
                assert_eq!(message.route.channel, "wechat");
                assert_eq!(message.route.thread_id, None);
                (
                    message.route.conversation_id.clone(),
                    message.message_id.clone(),
                    message.text.clone(),
                )
            })
            .collect()
    }

    fn receive_requests(&self) -> Vec<Request> {
        self.receive
            .requests()
            .into_iter()
            .map(Request::parse)
            .collect()
    }

    fn receive_bodies(&self) -> Vec<Value> {
        self.receive_requests()
            .iter()
            .map(|request| serde_json::from_str(&request.body).expect("JSON poll"))
            .collect()
    }

    fn send_requests(&self) -> Vec<Request> {
        self.send
            .requests()
            .into_iter()
            .map(Request::parse)
            .collect()
    }

    async fn stored(&self, key: &'static str) -> Option<String> {
        self.storage
            .get_bytes(key)
            .await
            .expect("read storage")
            .map(|bytes| String::from_utf8(bytes).expect("UTF-8 value"))
    }

    fn config(&self) -> ConfigEndpoint<S, ScriptedStack, ScriptedStack, ScriptedStack> {
        ConfigEndpoint {
            configuration: Rc::clone(&self.channel),
        }
    }

    fn registered(&self) -> bool {
        self.gateway.register(Rc::new(OtherWechatChannel)).is_err()
    }

    fn state(&self) -> ReceiveState {
        self.channel.receive.state()
    }

    async fn call(
        &self,
        endpoint: &dyn HttpEndpoint,
        method: HttpMethod,
        body: &[u8],
    ) -> (u16, Value) {
        self.request(endpoint, HttpRequest::new(method, body.to_vec()))
            .await
    }

    /// `GET /api/gateway/wechat/status`.
    async fn status(&self) -> (u16, Value) {
        let routes = ChannelEndpoint::new(Rc::clone(&self.channel), CONFIG_API_PATH, self.config());
        let path = format!("{CONFIG_API_PATH}/status");
        self.request(
            &routes,
            HttpRequest::with_path(HttpMethod::Get, path, Vec::new()),
        )
        .await
    }

    async fn request(&self, endpoint: &dyn HttpEndpoint, request: HttpRequest) -> (u16, Value) {
        let response = endpoint.handle(request).await;
        let body = response.body().expect("buffered response");
        let json = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body).expect("JSON response")
        };
        (response.status(), json)
    }
}

struct Request {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.split("\r\n");
        let path = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .into();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.into(), value.trim().into()))
            .collect();
        Self {
            path,
            headers,
            body: body.into(),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON request")
    }
}

fn plugin_context() -> PluginContext {
    let stack = never_embassy_stack();
    let info = barracuda_plugin::api::TargetIdentity::new(
        barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
        barracuda_plugin::api::BoardInfo::new(
            "test-board",
            barracuda_plugin::api::Hardware::new("test-chip"),
        ),
    );
    PluginContext::new(info, stack, ClientFactory::plaintext(stack))
}

/// Options of one scenario.
struct Setup {
    stored: Stored,
    receive: Vec<ScriptStep>,
    send: Vec<ScriptStep>,
    /// Holds the only slot before the channel loads.
    slot_taken: bool,
}

impl Setup {
    fn new(stored: Stored, receive: Vec<ScriptStep>) -> Self {
        Self {
            stored,
            receive,
            send: Vec::new(),
            slot_taken: false,
        }
    }
}

/// Loads the channel over a one-slot receive pool and runs `scenario`
/// beside its receive runtime, as the Plugin's task would.
fn scenario(setup: Setup, scenario: impl Scenario) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let mut context = plugin_context();
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow Plugin");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register IMessage Gateway Plugin");
    manager
        .register(Probe {
            setup: Some(setup),
            scenario: Some(scenario),
        })
        .expect("run the receive scenario");
}

/// Declares a [`Scenario`] named `$name` whose body sees `$harness`.
macro_rules! scenario {
    ($name:ident, |$harness:ident| $body:block) => {
        struct $name;

        impl Scenario for $name {
            async fn run<S: PluginStorage>(self, $harness: Rc<Harness<S>>) $body
        }
    };
}

fn text(message_id: u64, from: &str, text: &str, context_token: &str) -> Value {
    json!({
        "seq": message_id,
        "message_id": message_id,
        "from_user_id": from,
        "to_user_id": "bot@im.bot",
        "create_time_ms": 1_700_000_000_000_u64,
        "message_type": 1,
        "message_state": 2,
        "context_token": context_token,
        "item_list": [{ "type": 1, "text_item": { "text": text } }],
    })
}

fn updates(cursor: &str, msgs: Vec<Value>) -> ScriptStep {
    ScriptStep::json(
        200,
        &json!({
            "ret": 0,
            "msgs": msgs,
            "get_updates_buf": cursor,
            "longpolling_timeout_ms": 35000,
        })
        .to_string(),
    )
}

/// A long poll the server holds until the test ends.
fn hold() -> ScriptStep {
    ScriptStep::pending_after_headers(200, "application/json")
}

fn sent() -> ScriptStep {
    ScriptStep::json(200, r#"{"ret":0}"#)
}

#[test]
fn owner_text_is_published_on_the_sender_route() {
    let receive = Vec::from([
        updates(
            "cursor-1",
            Vec::from([
                text(18_446_744_073_709_551_615, OWNER, "hello", "ctx-1"),
                json!({ "message_id": 2, "from_user_id": "bot@im.bot", "message_type": 2,
                        "item_list": [{ "type": 1, "text_item": { "text": "echo" } }] }),
                json!({ "message_id": 3, "from_user_id": OWNER, "message_type": 1,
                        "item_list": [{ "type": 2, "image_item": {} }] }),
            ]),
        ),
        hold(),
    ]);
    let mut setup = Setup::new(Stored::linked().with_owner(OWNER), receive);
    setup.send = Vec::from([sent()]);
    scenario!(Body, |harness| {
        harness
            .until("the second poll", |harness| {
                harness.receive.requests().len() == 2
            })
            .await;

        assert_eq!(
            harness.published(),
            [(OWNER.into(), "18446744073709551615".into(), "hello".into())]
        );
        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.channel.inbound.skipped.get(), 2);
        let requests = harness.receive_requests();
        let first = &requests[0];
        assert_eq!(first.path, "/ilink/bot/getupdates");
        assert_eq!(first.header("Authorization"), Some("Bearer bot-token"));
        assert_eq!(first.header("AuthorizationType"), Some("ilink_bot_token"));
        assert_eq!(first.header("iLink-App-Id"), Some("bot"));
        assert_eq!(first.header("iLink-App-ClientVersion"), Some("131329"));
        let uins: Vec<&str> = requests
            .iter()
            .map(|request| request.header("X-WECHAT-UIN").expect("UIN header"))
            .collect();
        assert_ne!(uins[0], uins[1], "a new UIN per request");
        assert_eq!(first.json()["get_updates_buf"], "");
        assert_eq!(requests[1].json()["get_updates_buf"], "cursor-1");
        assert_eq!(
            harness.receive.connect_count(),
            1,
            "one kept-alive connection"
        );
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-1")
        );
        let tokens: Value = serde_json::from_str(
            &harness
                .stored(CONTEXT_TOKENS_STORAGE_KEY)
                .await
                .expect("tokens stored"),
        )
        .expect("token JSON");
        assert_eq!(tokens, json!([{ "user": OWNER, "token": "ctx-1" }]));

        // A reply on the route carries the stored token without a thread.
        let provider = harness.channel.provider().expect("linked");
        provider
            .send_message(SendMessageRequest::text(
                MessageTarget::new("wechat", OWNER),
                "hi back",
            ))
            .await
            .expect("send");
        let reply = harness.send_requests().pop().expect("send request").json();
        assert_eq!(reply["msg"]["to_user_id"], OWNER);
        assert_eq!(reply["msg"]["context_token"], "ctx-1");
    });
    scenario(setup, Body);
}

#[test]
fn pairing_code_adds_the_sender_and_replies_once() {
    let receive = Vec::from([
        updates(
            "cursor-1",
            Vec::from([text(7, OWNER, PAIRING_CODE, "ctx-7")]),
        ),
        hold(),
    ]);
    let mut setup = Setup::new(Stored::linked(), receive);
    setup.send = Vec::from([sent()]);
    scenario!(Body, |harness| {
        harness
            .until("the pairing reply", |harness| {
                !harness.send_requests().is_empty()
            })
            .await;

        assert!(harness.channel.owners().expect("owners").is_owner(OWNER));
        assert!(harness.published().is_empty(), "the code is not published");
        let reply = harness.send_requests().remove(0).json();
        assert_eq!(reply["msg"]["to_user_id"], OWNER);
        assert_eq!(reply["msg"]["context_token"], "ctx-7");
        assert_eq!(
            reply["msg"]["item_list"][0]["text_item"]["text"],
            PAIRED_REPLY
        );
    });
    scenario(setup, Body);
}

#[test]
fn strangers_are_dropped_without_a_reply_or_token() {
    let receive = Vec::from([
        updates("cursor-1", Vec::from([text(9, STRANGER, "hi", "ctx-9")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the second poll", |harness| {
                harness.receive.requests().len() == 2
            })
            .await;

        assert!(harness.published().is_empty());
        assert!(harness.send_requests().is_empty());
        assert_eq!(harness.channel.owners().expect("owners").ignored(), 1);
        assert_eq!(harness.channel.inbound.context_tokens.get(STRANGER), None);
        assert_eq!(harness.stored(CONTEXT_TOKENS_STORAGE_KEY).await, None);
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn redelivered_messages_are_dropped_after_a_reconnect() {
    let receive = Vec::from([
        updates("cursor-1", Vec::from([text(1, OWNER, "one", "ctx")])),
        // The server closes the kept-alive connection.
        ScriptStep::ConnectError(ErrorKind::ConnectionReset),
        updates(
            "cursor-2",
            Vec::from([text(1, OWNER, "one", "ctx"), text(2, OWNER, "two", "ctx")]),
        ),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the poll after both messages", |harness| {
                harness.receive.requests().len() == 4
            })
            .await;

        assert_eq!(
            harness.published(),
            [
                (OWNER.into(), "1".into(), "one".into()),
                (OWNER.into(), "2".into(), "two".into()),
            ]
        );
        assert_eq!(harness.receive.connect_count(), 2, "reconnected at once");
        assert_eq!(harness.state(), ReceiveState::Receiving);
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn cursor_is_restored_and_idle_cursors_are_written_sparingly() {
    let receive = Vec::from([
        updates("cursor-1", Vec::from([text(1, OWNER, "one", "ctx")])),
        updates("cursor-2", Vec::new()),
        hold(),
    ]);
    let stored = Stored {
        cursor: Some("cursor-0"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .until("the third poll", |harness| {
                harness.receive.requests().len() == 3
            })
            .await;

        let bodies = harness.receive_bodies();
        assert_eq!(bodies[0]["get_updates_buf"], "cursor-0");
        assert_eq!(bodies[1]["get_updates_buf"], "cursor-1");
        assert_eq!(bodies[2]["get_updates_buf"], "cursor-2");
        // A batch with messages is stored at once; an empty one waits.
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-1")
        );
    });
    scenario(Setup::new(stored, receive), Body);
}

#[test]
fn an_oversized_batch_is_skipped_past_its_cursor_and_counted() {
    let huge = "x".repeat(wechat::MAX_UPDATES_BYTES);
    let receive = Vec::from([
        updates("cursor-big", Vec::from([text(1, OWNER, &huge, "ctx")])),
        updates("cursor-2", Vec::from([text(2, OWNER, "after", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the message after the skipped batch", |harness| {
                !harness.published().is_empty()
            })
            .await;

        assert_eq!(
            harness.published(),
            [(OWNER.into(), "2".into(), "after".into())]
        );
        assert_eq!(harness.channel.inbound.oversized.get(), 1);
        assert_eq!(harness.state(), ReceiveState::Receiving);
        let bodies = harness.receive_bodies();
        assert_eq!(bodies[1]["get_updates_buf"], "cursor-big");
        assert_eq!(bodies[2]["get_updates_buf"], "cursor-2");
        assert_eq!(harness.receive.connect_count(), 1, "the connection is kept");
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-2")
        );
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn an_unanswered_long_poll_is_an_empty_success() {
    let short = ScriptStep::json(
        200,
        r#"{"ret":0,"msgs":[],"get_updates_buf":"cursor-1","longpolling_timeout_ms":5}"#,
    );
    let receive = Vec::from([
        short,
        hold(),
        updates("cursor-2", Vec::from([text(4, OWNER, "late", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the message after the timeout", |harness| {
                !harness.published().is_empty()
            })
            .await;

        assert_eq!(harness.receive.connect_count(), 2);
        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.receive_bodies()[2]["get_updates_buf"], "cursor-1");
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn ilink_errors_retry_after_backoff() {
    let receive = Vec::from([
        ScriptStep::json(200, r#"{"ret":-1,"errmsg":"busy"}"#),
        updates("cursor-1", Vec::from([text(5, OWNER, "after", "ctx")])),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("the retried poll", |harness| {
                !harness.published().is_empty()
            })
            .await;

        assert_eq!(harness.state(), ReceiveState::Receiving);
        assert_eq!(harness.receive.connect_count(), 2);
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn an_expired_session_halts_until_a_new_login() {
    let receive = Vec::from([
        updates("cursor-1", Vec::new()),
        ScriptStep::json(200, r#"{"ret":-14,"errmsg":"session timeout"}"#),
        updates("cursor-9", Vec::from([text(6, OWNER, "back", "ctx")])),
        hold(),
    ]);
    let stored = Stored {
        cursor: Some("cursor-0"),
        bot_id: Some("bot-1"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .until("the halt", |harness| {
                matches!(harness.state(), ReceiveState::Error(_))
            })
            .await;
        harness.settle().await;

        assert_eq!(
            harness.state(),
            ReceiveState::Error(SESSION_EXPIRED_MESSAGE.into())
        );
        assert_eq!(harness.receive.requests().len(), 2, "no retry while halted");
        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(
            body["receive"],
            json!({
                "state": "error",
                "message": SESSION_EXPIRED_MESSAGE,
                "slots": { "in_use": 1, "capacity": 1 },
            })
        );
        assert_eq!(
            entry_status(&harness.channel, &LoginWatch::idle()),
            EntryStatus::new(
                EntryState::Attention,
                WebText {
                    zh: "需要重新扫码",
                    en: "Scan again to relink",
                },
            )
        );

        // A QR login confirmed for another bot clears the old bot's state.
        harness
            .channel
            .inbound
            .context_tokens
            .remember(OWNER, "old");
        let config = ConfigRequest::from_login("new-token".into(), API_BASE.into());
        assert!(harness.channel.link(config, Some("bot-2")).await.is_ok());
        harness
            .until("receiving again", |harness| !harness.published().is_empty())
            .await;

        let requests = harness.receive_requests();
        let resumed = requests.get(2).expect("a poll after the login");
        assert_eq!(resumed.header("Authorization"), Some("Bearer new-token"));
        assert_eq!(
            resumed.json()["get_updates_buf"],
            "",
            "the cursor was cleared"
        );
        assert_eq!(
            harness.stored(BOT_ID_STORAGE_KEY).await.as_deref(),
            Some("bot-2")
        );
        assert_eq!(
            harness.channel.inbound.context_tokens.get(OWNER).as_deref(),
            Some("ctx"),
            "only the new bot's token is left"
        );
        assert_eq!(harness.state(), ReceiveState::Receiving);
    });
    scenario(Setup::new(stored, receive), Body);
}

#[test]
fn a_new_login_for_the_same_bot_keeps_the_cursor() {
    let stored = Stored {
        cursor: Some("cursor-0"),
        bot_id: Some("bot-1"),
        ..Stored::linked().with_owner(OWNER)
    };
    scenario!(Body, |harness| {
        harness
            .channel
            .inbound
            .context_tokens
            .remember(OWNER, "ctx");
        let config = ConfigRequest::from_login("new-token".into(), API_BASE.into());
        assert!(harness.channel.link(config, Some("bot-1")).await.is_ok());

        assert_eq!(*harness.channel.inbound.cursor.borrow(), "cursor-0");
        assert_eq!(
            harness.channel.inbound.context_tokens.get(OWNER).as_deref(),
            Some("ctx")
        );
        assert_eq!(
            harness.stored(CURSOR_STORAGE_KEY).await.as_deref(),
            Some("cursor-0")
        );
    });
    scenario(Setup::new(stored, Vec::new()), Body);
}

#[test]
fn disabling_stops_the_loop_frees_the_slot_and_unregisters() {
    let receive = Vec::from([
        updates("cursor-1", Vec::new()),
        hold(),
        updates("cursor-2", Vec::new()),
        hold(),
    ]);
    scenario!(Body, |harness| {
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;
        assert_eq!(harness.slots.in_use(), 1);
        assert!(harness.registered());
        let mode = ModeEndpoint::new(Rc::clone(&harness.channel));

        let (code, _body) = harness
            .call(&mode, HttpMethod::Post, br#"{"mode":"disabled"}"#)
            .await;
        assert_eq!(code, 204);
        harness.settle().await;

        assert_eq!(harness.state(), ReceiveState::Idle);
        assert_eq!(harness.slots.in_use(), 0);
        assert!(!harness.registered());
        let polls = harness.receive.requests().len();
        assert_eq!(
            harness.stored("mode").await.as_deref(),
            Some("\"disabled\"")
        );

        let (code, body) = harness
            .call(&mode, HttpMethod::Post, br#"{"mode":"send"}"#)
            .await;
        assert_eq!((code, body), (400, json!({ "error": "unsupported_mode" })));

        let (code, _body) = harness
            .call(&mode, HttpMethod::Post, br#"{"mode":"send_receive"}"#)
            .await;
        assert_eq!(code, 204);
        harness
            .until("polling again", |harness| {
                harness.receive.requests().len() > polls
            })
            .await;
        assert!(harness.registered());
        assert_eq!(harness.slots.in_use(), 1);
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn configurations_from_before_modes_receive() {
    scenario!(Body, |harness| {
        assert_eq!(harness.channel.mode(), ChannelMode::SendReceive);
        assert!(harness.registered());
        harness
            .until("the first poll", |harness| {
                harness.receive.requests().len() == 1
            })
            .await;
    });
    for mode in [None, Some("send")] {
        let stored = Stored {
            mode,
            ..Stored::linked()
        };
        scenario(Setup::new(stored, Vec::from([hold()])), Body);
    }
}

#[test]
fn without_a_free_slot_it_waits_and_starts_when_one_frees() {
    let receive = Vec::from([updates("cursor-1", Vec::new()), hold()]);
    let mut setup = Setup::new(Stored::linked().with_owner(OWNER), receive);
    setup.slot_taken = true;
    scenario!(Body, |harness| {
        harness.settle().await;
        assert_eq!(harness.state(), ReceiveState::NoSlot);
        assert!(harness.receive.requests().is_empty());
        let (_code, body) = harness.status().await;
        assert_eq!(
            body["receive"],
            json!({ "state": "no_slot", "capacity": 1, "slots": { "in_use": 1, "capacity": 1 } })
        );
        let mode = ModeEndpoint::new(Rc::clone(&harness.channel));
        let (code, body) = harness
            .call(&mode, HttpMethod::Post, br#"{"mode":"send_receive"}"#)
            .await;
        assert_eq!(
            (code, body),
            (409, json!({ "error": "no_slot", "capacity": 1 }))
        );

        // The retry timer is an hour away: only the freed slot wakes it.
        harness.blocker.take();
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;
        assert_eq!(harness.slots.in_use(), 1);
    });
    scenario(setup, Body);
}

#[test]
fn endpoints_report_status_and_owners() {
    let receive = Vec::from([updates("cursor-1", Vec::new()), hold()]);
    scenario!(Body, |harness| {
        harness
            .until("receiving", |harness| {
                harness.state() == ReceiveState::Receiving
            })
            .await;

        let (code, body) = harness.status().await;
        assert_eq!(code, 200);
        assert_eq!(
            body,
            json!({
                "configured": true,
                "mode": "send_receive",
                "receive": { "state": "receiving", "slots": { "in_use": 1, "capacity": 1 } },
                "owners": { "count": 1 },
            })
        );
        assert!(!body.to_string().contains("bot-token"));
        assert_eq!(
            entry_status(&harness.channel, &LoginWatch::idle()),
            EntryStatus::new(
                EntryState::Ready,
                WebText {
                    zh: "收发中",
                    en: "Receiving",
                },
            )
        );

        let owners = OwnersEndpoint::new(Rc::clone(&harness.channel));
        let (code, body) = harness.call(&owners, HttpMethod::Get, b"").await;
        assert_eq!(code, 200);
        assert_eq!(body["owners"], json!([{ "id": OWNER, "label": null }]));
        assert_eq!(body["pairing"]["code"], PAIRING_CODE);
        assert_eq!(body["ignored"], 0);

        let remove = format!(r#"{{"remove":"{OWNER}"}}"#);
        let (code, _body) = harness
            .call(&owners, HttpMethod::Post, remove.as_bytes())
            .await;
        assert_eq!(code, 204);
        assert_eq!(harness.channel.owners().expect("owners").count(), 0);
    });
    scenario(
        Setup::new(Stored::linked().with_owner(OWNER), receive),
        Body,
    );
}

#[test]
fn unconfigured_channel_does_not_receive() {
    scenario!(Body, |harness| {
        harness.settle().await;
        assert_eq!(harness.state(), ReceiveState::Idle);
        assert_eq!(harness.slots.in_use(), 0);
        assert!(!harness.registered());
        let (_code, body) = harness.status().await;
        assert_eq!(body["configured"], false);
        assert_eq!(
            entry_status(&harness.channel, &LoginWatch::idle()),
            EntryStatus::configured(false)
        );
    });
    scenario(Setup::new(Stored::default(), Vec::new()), Body);
}
