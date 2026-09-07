//! Plugin entry point for the direct-action Workflow runtime.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_vfs::{FsError, ScopedVfs};
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

pub use barracuda_workflow_runtime::*;

const WORKFLOW_CATALOG_PATH: &str = "/data/workflows.json";

#[derive(Clone)]
struct PersistedWorkflow {
    id: WorkflowId,
    json: Vec<u8>,
}

/// Runtime control and Event-emission capability published by the Plugin.
#[derive(Clone)]
pub struct WorkflowService {
    filesystem: ScopedVfs,
    actions: WorkflowActionRegistry,
    control: WorkflowRuntimeControl,
    view: WorkflowRuntimeView,
    catalog: Rc<RefCell<Vec<PersistedWorkflow>>>,
    restored: Rc<Mutex<NoopRawMutex, bool>>,
}

impl WorkflowService {
    fn new(
        filesystem: ScopedVfs,
        actions: WorkflowActionRegistry,
        control: WorkflowRuntimeControl,
        view: WorkflowRuntimeView,
    ) -> Self {
        Self {
            filesystem,
            actions,
            control,
            view,
            catalog: Rc::new(RefCell::new(Vec::new())),
            restored: Rc::new(Mutex::new(false)),
        }
    }

    /// Durably validates and loads one complete Workflow JSON document.
    pub async fn load(&self, json: &str) -> Result<(), WorkflowServiceError> {
        let mut restored = self.restored.lock().await;
        self.ensure_restored(&mut restored).await?;
        let definition = parse_definition(json).map_err(WorkflowServiceError::Rejected)?;
        validate_definition(&self.actions, &definition).map_err(WorkflowServiceError::Rejected)?;
        if self.control.contains(definition.id()) {
            return Err(WorkflowServiceError::Rejected(
                WorkflowControlRejection::DuplicateId,
            ));
        }

        let mut next_catalog = self.catalog.borrow().clone();
        next_catalog.push(PersistedWorkflow {
            id: definition.id().clone(),
            json: json.as_bytes().to_vec(),
        });
        self.write_catalog(&next_catalog).await?;
        self.control.load(definition).map_err(|_error| {
            WorkflowServiceError::Rejected(WorkflowControlRejection::DuplicateId)
        })?;
        *self.catalog.borrow_mut() = next_catalog;
        Ok(())
    }

    /// Validates and loads one Workflow for this boot without changing the
    /// durable user catalog.
    pub async fn load_transient(&self, json: &str) -> Result<(), WorkflowServiceError> {
        let mut restored = self.restored.lock().await;
        self.ensure_restored(&mut restored).await?;
        let definition = parse_definition(json).map_err(WorkflowServiceError::Rejected)?;
        validate_definition(&self.actions, &definition).map_err(WorkflowServiceError::Rejected)?;
        self.control
            .load(definition)
            .map_err(|_error| WorkflowServiceError::Rejected(WorkflowControlRejection::DuplicateId))
    }

    /// Durably unloads one Workflow without cancelling running snapshots.
    pub async fn unload(&self, workflow_id: &WorkflowId) -> Result<(), WorkflowServiceError> {
        let mut restored = self.restored.lock().await;
        self.ensure_restored(&mut restored).await?;
        if !self.control.contains(workflow_id) {
            return Err(WorkflowServiceError::Rejected(
                WorkflowControlRejection::NotFound,
            ));
        }
        let mut next_catalog = self.catalog.borrow().clone();
        let Some(position) = next_catalog
            .iter()
            .position(|workflow| &workflow.id == workflow_id)
        else {
            return Err(WorkflowServiceError::Rejected(
                WorkflowControlRejection::Persistence,
            ));
        };
        next_catalog.remove(position);
        self.write_catalog(&next_catalog).await?;
        self.control
            .unload(workflow_id)
            .map_err(|_error| WorkflowServiceError::Rejected(WorkflowControlRejection::NotFound))?;
        *self.catalog.borrow_mut() = next_catalog;
        Ok(())
    }

    /// Returns loaded definitions in load order.
    #[must_use]
    pub fn definitions(&self) -> Vec<WorkflowDefinition> {
        self.view.definitions()
    }

    /// Returns current execution counters and the latest failure.
    #[must_use]
    pub fn info(&self) -> WorkflowInfo {
        self.view.info()
    }

    /// Emits one typed Event directly into Workflow matching.
    pub fn emit<E>(&self, input: WorkflowValue) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.control.emit::<E>(input)
    }

    /// Emits one typed Event with a topic filter.
    pub fn emit_to<E>(&self, topic: Topic, input: WorkflowValue) -> Result<(), EmitError>
    where
        E: Event,
    {
        self.control.emit_to::<E>(topic, input)
    }

    /// Emits an Event whose identity is selected at runtime.
    pub fn emit_event(&self, event_id: EventId, topic: Option<Topic>, input: WorkflowValue) {
        self.control.emit_event(event_id, topic, input);
    }

    async fn restore(&self) -> Result<(), WorkflowServiceError> {
        let mut restored = self.restored.lock().await;
        self.ensure_restored(&mut restored).await
    }

    async fn ensure_restored(&self, restored: &mut bool) -> Result<(), WorkflowServiceError> {
        if !*restored {
            self.restore_catalog().await?;
            *restored = true;
        }
        Ok(())
    }

    async fn restore_catalog(&self) -> Result<(), WorkflowServiceError> {
        let bytes = match self.filesystem.read(WORKFLOW_CATALOG_PATH).await {
            Ok(bytes) => bytes,
            Err(FsError::NotFound) => {
                self.filesystem
                    .write_atomic(WORKFLOW_CATALOG_PATH, b"[]")
                    .await?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let documents: Vec<WorkflowValue> = serde_json::from_slice(&bytes).map_err(|_error| {
            WorkflowServiceError::InvalidCatalog(WorkflowControlRejection::InvalidJson)
        })?;
        let mut catalog = Vec::with_capacity(documents.len());
        for document in documents {
            let json = serde_json::to_vec(&document).map_err(|_error| {
                WorkflowServiceError::InvalidCatalog(WorkflowControlRejection::InvalidJson)
            })?;
            let source = core::str::from_utf8(&json).map_err(|_error| {
                WorkflowServiceError::InvalidCatalog(WorkflowControlRejection::InvalidJson)
            })?;
            let definition =
                parse_definition(source).map_err(WorkflowServiceError::InvalidCatalog)?;
            let id = definition.id().clone();
            self.control.load(definition).map_err(|_error| {
                WorkflowServiceError::InvalidCatalog(WorkflowControlRejection::DuplicateId)
            })?;
            catalog.push(PersistedWorkflow { id, json });
        }
        *self.catalog.borrow_mut() = catalog;
        Ok(())
    }

    async fn write_catalog(
        &self,
        catalog: &[PersistedWorkflow],
    ) -> Result<(), WorkflowServiceError> {
        self.filesystem
            .write_atomic(WORKFLOW_CATALOG_PATH, &catalog_json(catalog))
            .await
            .map_err(WorkflowServiceError::from)
    }
}

fn catalog_json(catalog: &[PersistedWorkflow]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(b'[');
    for (index, workflow) in catalog.iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        bytes.extend_from_slice(&workflow.json);
    }
    bytes.push(b']');
    bytes
}

/// Failure returned by [`WorkflowService`] durable control operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowServiceError {
    /// Workflow validation or catalog semantics rejected the operation.
    #[error("Workflow control request was rejected: {0:?}")]
    Rejected(WorkflowControlRejection),
    /// The persisted catalog contained an invalid Workflow document.
    #[error("persisted Workflow catalog is invalid: {0:?}")]
    InvalidCatalog(WorkflowControlRejection),
    /// The private Workflow filesystem failed.
    #[error(transparent)]
    Filesystem(#[from] FsError),
}

/// Plugin that publishes Workflow Action registration and runtime control.
#[barracuda_plugin::macros::plugin]
pub struct WorkflowPlugin {
    runtime: Option<WorkflowRuntime>,
    service: Option<Rc<WorkflowService>>,
}

impl WorkflowPlugin {
    /// Creates the unregistered Workflow Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            runtime: None,
            service: None,
        }
    }
}

impl Plugin for WorkflowPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let actions = WorkflowActionRegistry::new();
        let runtime = WorkflowRuntime::new(actions.clone());
        let service = Rc::new(WorkflowService::new(
            context.filesystem()?.clone(),
            actions.clone(),
            runtime.control(),
            runtime.view(),
        ));
        context.provide(Rc::new(actions))?;
        context.provide(Rc::clone(&service))?;
        self.runtime = Some(runtime);
        self.service = Some(service);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(WorkflowRuntimeUnavailable))?;
        let service = self
            .service
            .take()
            .ok_or_else(|| PluginError::registration(WorkflowRuntimeUnavailable))?;
        let spawner = context.task_spawner()?;
        let cancellation = context.task_token();
        spawner
            .spawn(workflow_task(runtime, service, cancellation))
            .map_err(PluginError::registration)
    }
}

#[embassy_executor::task]
async fn workflow_task(
    mut runtime: WorkflowRuntime,
    service: Rc<WorkflowService>,
    cancellation: PluginTaskToken,
) {
    if let Err(error) = service.restore().await {
        log::error!("failed to restore Workflow catalog: {error}");
    }
    let _completed = select(cancellation.cancelled(), &mut runtime).await;
    log::info!("stopped Workflow runtime task");
}

#[derive(Debug, thiserror::Error)]
#[error("Workflow runtime was not prepared during Plugin registration")]
struct WorkflowRuntimeUnavailable;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use barracuda_platform_test::memory_vfs_root;
    use futures_lite::future::block_on;

    #[test]
    fn transient_load_before_task_start_does_not_change_the_user_catalog() {
        block_on(async {
            let root = memory_vfs_root().await.expect("memory filesystem");
            let filesystem = root.scoped("/workflow").expect("scope");
            filesystem
                .write(
                    WORKFLOW_CATALOG_PATH,
                    br#"[{"id":"saved","match":{"event":"test"},"steps":[{"return":{}}]}]"#,
                )
                .await
                .expect("persisted catalog");
            let actions = WorkflowActionRegistry::new();
            let runtime = WorkflowRuntime::new(actions.clone());
            let service = WorkflowService::new(
                filesystem.clone(),
                actions,
                runtime.control(),
                runtime.view(),
            );
            service
                .load_transient(
                    r#"{"id":"bundled","match":{"event":"test"},"steps":[{"return":{}}]}"#,
                )
                .await
                .expect("load transient definition before Workflow task starts");
            service
                .restore()
                .await
                .expect("task restoration is idempotent");
            assert_eq!(service.definitions().len(), 2);
            let persisted: Vec<WorkflowValue> = serde_json::from_slice(
                &filesystem
                    .read(WORKFLOW_CATALOG_PATH)
                    .await
                    .expect("catalog"),
            )
            .expect("valid catalog");
            assert_eq!(persisted.len(), 1);
            assert_eq!(persisted[0]["id"], "saved");
        });
    }

    #[test]
    fn persisted_user_definition_wins_over_a_transient_duplicate() {
        block_on(async {
            let root = memory_vfs_root().await.expect("memory filesystem");
            let filesystem = root.scoped("/workflow-user-precedence").expect("scope");
            let user = r#"{"id":"shared","match":{"event":"user"},"steps":[{"return":{}}]}"#;
            filesystem
                .write(WORKFLOW_CATALOG_PATH, alloc::format!("[{user}]").as_bytes())
                .await
                .expect("persisted user catalog");
            let actions = WorkflowActionRegistry::new();
            let runtime = WorkflowRuntime::new(actions.clone());
            let service = WorkflowService::new(
                filesystem.clone(),
                actions,
                runtime.control(),
                runtime.view(),
            );

            let error = service
                .load_transient(
                    r#"{"id":"shared","match":{"event":"bundled"},"steps":[{"return":{}}]}"#,
                )
                .await
                .expect_err("persisted user Workflow must keep its ID");

            assert!(matches!(
                error,
                WorkflowServiceError::Rejected(WorkflowControlRejection::DuplicateId)
            ));
            assert_eq!(service.definitions().len(), 1);
            assert_eq!(service.definitions()[0].event().as_str(), "user");
            assert_eq!(
                filesystem
                    .read(WORKFLOW_CATALOG_PATH)
                    .await
                    .expect("user catalog"),
                alloc::format!("[{user}]").as_bytes()
            );
        });
    }
}
