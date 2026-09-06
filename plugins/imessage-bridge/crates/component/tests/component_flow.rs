#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::cell::{Cell, RefCell};
use std::future::{pending, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::string::String;
use std::task::Poll;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, RegisterContext, RpcAddress,
    RpcError, RpcLaneStorage, RunContext, UnregisterContext,
};
use barracuda_imessage_bridge_component::ImessageBridgeComponent;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginError, PluginId, PluginManager, PluginRegisterContext,
    PluginResult,
};

const FRAME_SIZE: usize = 512;

struct TestBridgePlugin;

impl PluginDeclaration for TestBridgePlugin {
    const ID: &'static str = "imessage-bridge-test";
}

impl Plugin<FRAME_SIZE> for TestBridgePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let bridge = futures_lite::future::block_on(ImessageBridgeComponent::load(
            context.storage().clone(),
        ))
        .map_err(PluginError::registration)?;
        context.event_router.load(bridge)?;
        Ok(())
    }
}

enum DriverMode {
    Initial,
    Restored,
}

struct TestDriver {
    mode: DriverMode,
    completed: Rc<Cell<bool>>,
    failure: Rc<RefCell<Option<String>>>,
}

impl Component<FRAME_SIZE> for TestDriver {
    fn name(&self) -> &'static str {
        "imessage-bridge-test-driver"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let result = match self.mode {
                DriverMode::Initial => exercise_initial(context).await,
                DriverMode::Restored => exercise_restored(context).await,
            };
            if let Err(error) = result {
                self.failure.replace(Some(error.to_string()));
            }
            self.completed.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

async fn exercise_initial(context: RunContext<FRAME_SIZE>) -> Result<(), RpcError> {
    let client = context.rpc().clone();
    let to_agent = RpcAddress::try_from("imessage_bridge.to_agent")?;
    let to_gateway = RpcAddress::try_from("imessage_bridge.to_gateway")?;
    let public = client.rpcs_by_visibility("*")?;
    assert!(
        public
            .iter()
            .any(|address| address.as_ref() == to_agent.as_ref())
    );
    assert!(
        public
            .iter()
            .any(|address| address.as_ref() == to_gateway.as_ref())
    );

    let route = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-9","text":"hello"}"#;
    assert_call(&client, &to_agent, route, r#"{}"#).await?;
    let bind = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-9","session":"session-4"}"#;
    assert_call(&client, &to_agent, bind, r#"{"session":"session-4"}"#).await?;
    assert_call(
        &client,
        &to_agent,
        route,
        r#"{"open_required":false,"session":"session-4"}"#,
    )
    .await?;

    let queued_message = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-10","session":"session-4"}"#;
    assert_call(
        &client,
        &to_agent,
        queued_message,
        r#"{"session":"session-4"}"#,
    )
    .await?;

    let started = r#"{"session":"session-4","sequence":0,"type":"turn_started","payload":{"turn":"turn-1","origin":"user"}}"#;
    assert_call(
        &client,
        &to_gateway,
        started,
        r#"{"reply_to":"message-9","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let next_message = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-11","session":"session-4"}"#;
    assert_call(
        &client,
        &to_agent,
        next_message,
        r#"{"session":"session-4"}"#,
    )
    .await?;

    let reasoning = r#"{"session":"session-4","sequence":1,"type":"reasoning_delta","payload":{"text":"private"}}"#;
    assert_call(
        &client,
        &to_gateway,
        reasoning,
        r#"{"reply_to":"message-9","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let output =
        r#"{"session":"session-4","sequence":2,"type":"output_delta","payload":{"text":"world"}}"#;
    assert_call(
        &client,
        &to_gateway,
        output,
        r#"{"reply_to":"message-9","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let ended =
        r#"{"session":"session-4","sequence":3,"type":"turn_ended","payload":{"turn":"turn-1"}}"#;
    assert_call(
        &client,
        &to_gateway,
        ended,
        r#"{"reply_to":"message-9","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let next_started = r#"{"session":"session-4","sequence":4,"type":"turn_started","payload":{"turn":"turn-2","origin":"user"}}"#;
    assert_call(
        &client,
        &to_gateway,
        next_started,
        r#"{"reply_to":"message-10","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;
    let next_ended =
        r#"{"session":"session-4","sequence":5,"type":"turn_ended","payload":{"turn":"turn-2"}}"#;
    assert_call(
        &client,
        &to_gateway,
        next_ended,
        r#"{"reply_to":"message-10","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let third_started = r#"{"session":"session-4","sequence":6,"type":"turn_started","payload":{"turn":"turn-3","origin":"user"}}"#;
    assert_call(
        &client,
        &to_gateway,
        third_started,
        r#"{"reply_to":"message-11","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;
    let third_ended =
        r#"{"session":"session-4","sequence":7,"type":"turn_ended","payload":{"turn":"turn-3"}}"#;
    assert_call(
        &client,
        &to_gateway,
        third_ended,
        r#"{"reply_to":"message-11","route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let tool_started = r#"{"session":"session-4","sequence":8,"type":"turn_started","payload":{"turn":"turn-4","origin":"tool_call"}}"#;
    assert_call(
        &client,
        &to_gateway,
        tool_started,
        r#"{"reply_to":null,"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;
    let tool_ended =
        r#"{"session":"session-4","sequence":9,"type":"turn_ended","payload":{"turn":"turn-4"}}"#;
    assert_call(
        &client,
        &to_gateway,
        tool_ended,
        r#"{"reply_to":null,"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"}}"#,
    )
    .await?;

    let closed =
        r#"{"session":"session-4","sequence":10,"type":"closed","payload":{"reason":"closed"}}"#;
    assert_call(&client, &to_gateway, closed, r#"{}"#).await?;
    assert_call(
        &client,
        &to_agent,
        route,
        r#"{"open_required":true,"session":"session-4"}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"session":"session-99","sequence":0,"type":"turn_started","payload":{"turn":"turn-1","origin":"user"}}"#,
        r#"{}"#,
    )
    .await?;
    assert!(client.call_json(&to_agent, "[]")?.await.is_err());
    Ok(())
}

async fn exercise_restored(context: RunContext<FRAME_SIZE>) -> Result<(), RpcError> {
    let address = RpcAddress::try_from("imessage_bridge.to_agent")?;
    let route = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-10","text":"again"}"#;
    assert_call(
        context.rpc(),
        &address,
        route,
        r#"{"open_required":true,"session":"session-4"}"#,
    )
    .await
}

async fn assert_call(
    client: &barracuda_event_router::RpcClient,
    address: &RpcAddress,
    request: &str,
    expected: &str,
) -> Result<(), RpcError> {
    let response = client.call_json(address, request)?.await?;
    assert_eq!(response.as_str()?, expected);
    Ok(())
}

fn drive_until_complete(
    router: &mut EventRouter<8, FRAME_SIZE, 8>,
    completed: &Cell<bool>,
) -> Result<(), String> {
    futures_lite::future::block_on(poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut *router).poll(context) {
            return Poll::Ready(result.map_err(|error| error.to_string()));
        }
        if completed.get() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }))
}

#[test]
fn route_persists_and_turn_events_resolve_the_gateway_target() {
    futures_lite::future::block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition =
        futures_lite::future::block_on(memory_partition(64 * 1024)).expect("create test partition");
    let mut manager =
        futures_lite::future::block_on(PluginManager::open(partition)).expect("open manager");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<8, FRAME_SIZE, 8>::new()));
    let mut router = futures_lite::future::block_on(EventRouter::new(lanes)).expect("router");
    manager
        .register(&mut router, TestBridgePlugin)
        .expect("register bridge");

    let completed = Rc::new(Cell::new(false));
    let failure = Rc::new(RefCell::new(None));
    let driver = router
        .load(Box::new(TestDriver {
            mode: DriverMode::Initial,
            completed: Rc::clone(&completed),
            failure: Rc::clone(&failure),
        }))
        .expect("load driver");
    drive_until_complete(&mut router, &completed).expect("drive initial flow");
    assert_eq!(failure.borrow().as_deref(), None);
    router.unload(driver).expect("unload driver");

    let id = PluginId::try_from(TestBridgePlugin::ID).expect("plugin ID");
    futures_lite::future::block_on(manager.unload(&mut router, &id)).expect("unload bridge");
    manager
        .register(&mut router, TestBridgePlugin)
        .expect("restore bridge");

    completed.set(false);
    router
        .load(Box::new(TestDriver {
            mode: DriverMode::Restored,
            completed: Rc::clone(&completed),
            failure: Rc::clone(&failure),
        }))
        .expect("load restore driver");
    drive_until_complete(&mut router, &completed).expect("drive restored flow");
    assert_eq!(failure.borrow().as_deref(), None);
}
