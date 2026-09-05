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
    assert_call(
        &client,
        &to_agent,
        route,
        r#"{"found":false,"open_required":false}"#,
    )
    .await?;
    let bind = r#"{"route":{"channel":"imessage","conversation_id":"chat-7","thread_id":"thread-2"},"message_id":"message-9","session":"session-4"}"#;
    assert_call(&client, &to_agent, bind, r#"{"session":"session-4"}"#).await?;
    assert_call(
        &client,
        &to_agent,
        route,
        r#"{"found":true,"open_required":false,"session":"session-4"}"#,
    )
    .await?;

    let started = r#"{"session":"session-4","run":"run-2","sequence":0,"chunk_index":0,"field":"type","chunk":"turn_started","field_complete":true,"event_complete":false,"terminal":null}"#;
    assert_call(
        &client,
        &to_gateway,
        started,
        r#"{"command_id":"command-1","forward":true}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"command_id":"command-1"}"#,
        r#"{"action":"start","channel":"imessage","conversation_id":"chat-7","reply_to":"message-9","sequence":0,"stream_id":"session-4.run-2.0","thread_id":"thread-2"}"#,
    )
    .await?;

    let reasoning = r#"{"session":"session-4","run":"run-2","sequence":1,"chunk_index":0,"field":"type","chunk":"reasoning_delta","field_complete":true,"event_complete":false,"terminal":null}"#;
    assert_call(&client, &to_gateway, reasoning, r#"{"forward":false}"#).await?;
    let reasoning_text = r#"{"session":"session-4","run":"run-2","sequence":1,"chunk_index":1,"field":"text","chunk":"private","field_complete":true,"event_complete":true,"terminal":null}"#;
    assert_call(&client, &to_gateway, reasoning_text, r#"{"forward":false}"#).await?;
    let output_type = r#"{"session":"session-4","run":"run-2","sequence":2,"chunk_index":0,"field":"type","chunk":"output_delta","field_complete":true,"event_complete":false,"terminal":null}"#;
    assert_call(&client, &to_gateway, output_type, r#"{"forward":false}"#).await?;
    let text_more = r#"{"session":"session-4","run":"run-2","sequence":2,"chunk_index":1,"field":"text","chunk":"wor","field_complete":false,"event_complete":false,"terminal":null}"#;
    assert_call(
        &client,
        &to_gateway,
        text_more,
        r#"{"command_id":"command-2","forward":true}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"command_id":"command-2"}"#,
        r#"{"action":"chunk","boundary":"more","field":"text","sequence":1,"stream_id":"session-4.run-2.0","text":"wor"}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"command_id":"command-2"}"#,
        r#"{"error":"unknown_command"}"#,
    )
    .await?;

    let text_complete = r#"{"session":"session-4","run":"run-2","sequence":2,"chunk_index":2,"field":"text","chunk":"ld","field_complete":true,"event_complete":true,"terminal":null}"#;
    assert_call(
        &client,
        &to_gateway,
        text_complete,
        r#"{"command_id":"command-3","forward":true}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"command_id":"command-3"}"#,
        r#"{"action":"chunk","boundary":"complete","field":"text","sequence":2,"stream_id":"session-4.run-2.0","text":"ld"}"#,
    )
    .await?;

    let ended = r#"{"session":"session-4","run":"run-2","sequence":3,"chunk_index":0,"field":"type","chunk":"turn_ended","field_complete":true,"event_complete":false,"terminal":null}"#;
    assert_call(
        &client,
        &to_gateway,
        ended,
        r#"{"command_id":"command-4","forward":true}"#,
    )
    .await?;
    assert_call(
        &client,
        &to_gateway,
        r#"{"command_id":"command-4"}"#,
        r#"{"action":"finish","sequence":3,"stream_id":"session-4.run-2.0"}"#,
    )
    .await?;
    let closed = r#"{"session":"session-4","run":"run-2","sequence":4,"chunk_index":0,"field":"type","chunk":"closed","field_complete":true,"event_complete":false,"terminal":"closed"}"#;
    assert_call(&client, &to_gateway, closed, r#"{"forward":false}"#).await?;
    assert_call(
        &client,
        &to_agent,
        route,
        r#"{"found":true,"open_required":true,"session":"session-4"}"#,
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
        r#"{"found":true,"open_required":true,"session":"session-4"}"#,
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
fn route_persists_and_output_delta_becomes_gateway_stream_commands() {
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
