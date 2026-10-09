//! Ingress backpressure through `IMessageGateway::ready`.

#![allow(clippy::expect_used, clippy::panic, missing_docs)]

use std::cell::RefCell;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use barracuda_imessage_gateway_plugin::{
    GatewayInboundMessage, GatewayRoute, IMessageGateway, IMessageGatewayPlugin,
};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_workflow_plugin::{WorkflowPlugin, WorkflowService, EVENT_BACKLOG_LIMIT};
use futures_lite::future::block_on;
use http_client::ClientFactory;

type Captured = Rc<RefCell<Option<(Rc<IMessageGateway>, Rc<WorkflowService>)>>>;

struct Probe(Captured);

impl PluginDeclaration for Probe {
    const ID: &'static str = "ready-probe";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway", "workflow"];
}

impl Plugin for Probe {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let workflow = context.require::<WorkflowService>("workflow")?;
        self.0.replace(Some((gateway, workflow)));
        Ok(())
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

fn is_ready(future: impl Future<Output = ()>) -> bool {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut context) == Poll::Ready(())
}

fn message(id: usize) -> GatewayInboundMessage {
    GatewayInboundMessage {
        route: GatewayRoute::new("test", "conversation"),
        message_id: format!("m{id}"),
        text: "hello".into(),
    }
}

/// Registers Workflow and the Gateway without starting their tasks, so
/// published messages stay queued as Workflow executions.
fn scenario(test: impl AsyncFnOnce(&IMessageGateway, &WorkflowService)) {
    block_on(install_global_memory_vfs()).expect("install test VFS");
    let partition = block_on(memory_partition(64 * 1024)).expect("test database region");
    let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
    manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
    let mut context = plugin_context();
    let captured: Captured = Rc::new(RefCell::new(None));
    manager
        .register(WorkflowPlugin::new(&mut context))
        .expect("register Workflow");
    manager
        .register(IMessageGatewayPlugin::new(&mut context))
        .expect("register Gateway");
    manager
        .register(Probe(Rc::clone(&captured)))
        .expect("register probe");
    let (gateway, workflow) = captured.take().expect("probe captured capabilities");
    block_on(test(&gateway, &workflow));
}

#[test]
fn ready_resolves_without_a_listener() {
    scenario(async |gateway, _workflow| {
        for id in 0..EVENT_BACKLOG_LIMIT + 2 {
            assert!(is_ready(gateway.ready()));
            gateway.publish(message(id)).await.expect("publish");
        }
        assert!(is_ready(gateway.ready()), "unmatched Events never queue");
    });
}

#[test]
fn ready_waits_while_the_inbound_backlog_is_full() {
    scenario(async |gateway, workflow| {
        workflow
            .load_transient(
                r#"{"id":"inbound","match":{"event":"gateway.message.received"},"steps":[{"return":{}}]}"#,
            )
            .await
            .expect("load inbound Workflow");
        for id in 0..EVENT_BACKLOG_LIMIT {
            assert!(is_ready(gateway.ready()), "below the limit at {id}");
            gateway.publish(message(id)).await.expect("publish");
        }
        assert!(!is_ready(gateway.ready()), "the backlog is full");
    });
}
