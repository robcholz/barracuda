#![allow(missing_docs)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{
    AgentRuntime, ApiPurpose, BackendKind, ModelApiConfig, ModelApiFactory, RuntimeStorageConfig,
};
use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_gateway_agent_component::GatewayAgentBridge;
use barracuda_imessage_gateway_component::component::GatewayComponent;
use barracuda_imessage_gateway_component::gateway_message_received::GatewayInboundMessage;
use barracuda_imessage_gateway_component::route::GatewayRoute;
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{install_global_memory_vfs, memory_vfs, ScriptStep, ScriptedStack};
use futures_lite::StreamExt;
use gateway::{
    ChannelFuture, MessageChannel, MessageGateway, SendMessageRequest, SendReceipt,
    SendStreamField, SendStreamFrame, SendStreamRequest,
};
use static_cell::StaticCell;

static NETWORK: StaticCell<ScriptedStack> = StaticCell::new();

#[derive(Default)]
struct DeliveryState {
    route: RefCell<Option<(String, String, Option<String>)>>,
    reply_to: RefCell<Option<String>>,
    frames: RefCell<Vec<SendStreamFrame>>,
    completed: Cell<bool>,
}

struct RecordingChannel(Rc<DeliveryState>);

impl MessageChannel for RecordingChannel {
    fn channel(&self) -> &str {
        "test"
    }

    fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
        Box::pin(async { Ok(SendReceipt::new("unused")) })
    }

    fn send_stream(&self, mut request: SendStreamRequest) -> ChannelFuture<'_, SendReceipt> {
        let state = Rc::clone(&self.0);
        Box::pin(async move {
            state.route.replace(Some((
                request.target.channel,
                request.target.conversation_id,
                request.target.thread_id,
            )));
            state.reply_to.replace(request.reply_to);
            while let Some(frame) = request.frames.next().await {
                state.frames.borrow_mut().push(frame?);
            }
            state.completed.set(true);
            Ok(SendReceipt::new("reply-1"))
        })
    }
}

#[test]
fn inbound_event_flows_through_exactly_two_workflow_steps_into_gateway_stream() {
    futures_lite::future::block_on(async {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let network: &'static ScriptedStack =
            NETWORK.init(ScriptedStack::new([ScriptStep::sse(200, &[sse])]));
        let factory = ModelApiFactory::new(move || ModelApi::new(network, network, 4096, 1024));
        let (runtime, service) = AgentRuntime::<ScriptedStack, ScriptedStack>::new(
            memory_vfs().await.expect("memory VFS mounts"),
            RuntimeStorageConfig {
                persistence_root: "/agent".into(),
                skill_roots: Vec::new(),
            },
            factory,
        )
        .expect("build Agent runtime");
        runtime
            .set_api(
                ModelApiConfig::new(
                    BackendKind::OpenAiCompatible,
                    "test-key",
                    "test-model",
                    "http://example.invalid",
                ),
                ApiPurpose::RootAgent,
                true,
            )
            .expect("configure model API");

        let delivery = Rc::new(DeliveryState::default());
        let facade = MessageGateway::new();
        let _registration = facade
            .register(Rc::new(RecordingChannel(Rc::clone(&delivery))))
            .expect("register channel");
        let (gateway, ingress) = GatewayComponent::new(facade, 4);

        let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 16>::new()));
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let mut router = EventRouter::new(lanes).await.expect("build Event Router");
        router
            .load(Box::new(AgentComponent::new(runtime, service)))
            .expect("load Agent");
        router.load(Box::new(gateway)).expect("load Gateway");
        router
            .load(Box::new(GatewayAgentBridge::new()))
            .expect("load mapper");

        drive_until(&mut router, |router| {
            !router.workflow_definitions().is_empty()
        })
        .await;
        let definition = router
            .workflow_definitions()
            .into_iter()
            .next()
            .expect("bridge Workflow");
        assert_eq!(definition.steps().len(), 2);

        ingress
            .publish(GatewayInboundMessage {
                route: GatewayRoute::new("test", "conversation-1").with_thread("thread-7"),
                message_id: "incoming-1".into(),
                text: "hello".into(),
            })
            .await
            .expect("publish inbound message");
        drive_until(&mut router, |_router| delivery.completed.get()).await;

        assert_eq!(
            delivery.route.borrow().as_ref(),
            Some(&(
                "test".into(),
                "conversation-1".into(),
                Some("thread-7".into())
            ))
        );
        assert_eq!(delivery.reply_to.borrow().as_deref(), Some("incoming-1"));
        assert!(delivery
            .frames
            .borrow()
            .iter()
            .any(|frame| { frame.field == SendStreamField::Reasoning && frame.text == "think" }));
        assert!(delivery
            .frames
            .borrow()
            .iter()
            .any(|frame| { frame.field == SendStreamField::Text && frame.text == "answer" }));
        assert!(delivery
            .frames
            .borrow()
            .iter()
            .any(|frame| frame.field == SendStreamField::Event));
    });
}

async fn drive_until(
    router: &mut EventRouter<16, 512, 16>,
    ready: impl Fn(&EventRouter<16, 512, 16>) -> bool,
) {
    core::future::poll_fn(|context| {
        if let Poll::Ready(result) = Pin::new(&mut *router).poll(context) {
            if let Err(error) = result {
                panic!("Event Router failed: {error}");
            }
        }
        if ready(router) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}
