//! Workflow Plugin capability and durable control integration tests.

#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{
    install_global_memory_vfs, memory_partition, memory_vfs_root, never_embassy_stack,
};
use barracuda_plugin::api::{ClientFactory, PluginContext};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginId, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_workflow_plugin::{
    workflow_action_schema_inline, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistry, WorkflowActionSchema, WorkflowPlugin, WorkflowService,
};
use futures_lite::future::block_on;
use serde_json::Value;

const FRAME_SIZE: usize = 64;

struct EchoAction;

impl WorkflowActionHandler for EchoAction {
    type Request = Value;
    type Response = Value;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!("test.echo", "{}", "{}");

    fn invoke(&self, input: Value) -> WorkflowActionFuture<'_, Value> {
        Box::pin(async move { Ok(input) })
    }
}

struct Consumer {
    actions: Rc<RefCell<Option<Rc<WorkflowActionRegistry>>>>,
    service: Rc<RefCell<Option<Rc<WorkflowService>>>>,
}

impl PluginDeclaration for Consumer {
    const ID: &'static str = "workflow-test-consumer";
    const DEPENDS_ON: &'static [&'static str] = &["workflow"];
}

impl Plugin<FRAME_SIZE> for Consumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let registration = actions
            .add_action(EchoAction)
            .expect("register dummy Workflow Action");
        context.retain(registration);
        *self.actions.borrow_mut() = Some(actions);
        *self.service.borrow_mut() = Some(context.require::<WorkflowService>("workflow")?);
        Ok(())
    }
}

#[test]
fn plugin_publishes_direct_action_and_runtime_control_capabilities() {
    block_on(async {
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create database partition");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        manager.install_vfs(memory_vfs_root().await.expect("create System VFS"));
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create Event Router");
        let stack = never_embassy_stack();
        let mut plugin_context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        let actions = Rc::new(RefCell::new(None));
        let service = Rc::new(RefCell::new(None));

        manager
            .register(&mut router, WorkflowPlugin::new(&mut plugin_context))
            .expect("register Workflow Plugin");
        manager
            .register(
                &mut router,
                Consumer {
                    actions: Rc::clone(&actions),
                    service: Rc::clone(&service),
                },
            )
            .expect("register Workflow capability consumer");

        assert_eq!(
            actions
                .borrow()
                .as_ref()
                .expect("Action registry capability")
                .descriptors()
                .len(),
            1
        );
        let service = service
            .borrow()
            .as_ref()
            .expect("Workflow service capability")
            .clone();
        service
            .load(
                r#"{"id":"mutable","match":{"event":"test.event"},"steps":[{"call":"test.echo"}]}"#,
            )
            .await
            .expect("durably load Workflow at runtime");
        assert_eq!(service.definitions().len(), 1);
        let id =
            barracuda_workflow_plugin::WorkflowId::try_from("mutable").expect("valid Workflow ID");
        service
            .unload(&id)
            .await
            .expect("durably unload Workflow at runtime");
        assert!(service.definitions().is_empty());

        let workflow_id = PluginId::try_from("workflow").expect("valid Plugin ID");
        assert_eq!(manager.component_ids(&workflow_id).map(<[_]>::len), Some(0));
    });
}
