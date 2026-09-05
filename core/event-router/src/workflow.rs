//! Durable Workflow Runtime adapter owned by Event Router.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_rpc::RpcContext;
use barracuda_vfs::{create_dir_all, read, remove_file, rename, write, FsError};
use barracuda_workflow::integration::{
    InternalEmit, WorkflowJsonRequest, WorkflowLoad, WorkflowRuntime, WorkflowRuntimeControl,
    WorkflowRuntimeView, WorkflowUnload,
};
use barracuda_workflow::{validate_definition, WorkflowControlRejection, WorkflowId};

use crate::EventRouterCreateError;

const WORKFLOW_CATALOG_PATH: &str = "/system/workflows.json";
const WORKFLOW_CATALOG_TEMP_PATH: &str = "/system/.workflows.json.tmp";
const SYSTEM_DIRECTORY: &str = "/system";

#[derive(Clone)]
struct PersistedWorkflow {
    id: WorkflowId,
    json: Vec<u8>,
}

pub(super) struct WorkflowComponent {
    runtime: WorkflowRuntime,
    catalog: Rc<RefCell<Vec<PersistedWorkflow>>>,
}

impl WorkflowComponent {
    pub(super) async fn new() -> Result<(Self, WorkflowRuntimeView), EventRouterCreateError> {
        create_dir_all(SYSTEM_DIRECTORY).await?;
        let runtime = WorkflowRuntime::new();
        let view = runtime.view();
        let catalog = restore(&runtime.control()).await?;
        Ok((
            Self {
                runtime,
                catalog: Rc::new(RefCell::new(catalog)),
            },
            view,
        ))
    }
}

impl<const M: usize> Component<M> for WorkflowComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context
            .register_json::<InternalEmit<M>, _>("system", self.runtime.ingress_handler::<M>())?;

        let load_control = self.runtime.control();
        let load_catalog = Rc::clone(&self.catalog);
        context.register_rpc::<WorkflowLoad<M>, _>(
            "system",
            move |context: RpcContext, frames| {
                let control = load_control.clone();
                let catalog = Rc::clone(&load_catalog);
                async move {
                    let request = match WorkflowJsonRequest::accept(frames).await? {
                        Ok(request) => request,
                        Err(rejection) => return Ok(Err(rejection)),
                    };
                    let definition = match request.definition() {
                        Ok(definition) => definition,
                        Err(rejection) => return Ok(Err(rejection)),
                    };
                    if let Err(rejection) = validate_definition(context.client(), &definition) {
                        return Ok(Err(rejection));
                    }
                    if control.contains(definition.id()) {
                        return Ok(Err(WorkflowControlRejection::DuplicateId));
                    }

                    let mut next_catalog = catalog.borrow().clone();
                    next_catalog.push(PersistedWorkflow {
                        id: definition.id().clone(),
                        json: request.bytes().to_vec(),
                    });
                    if write_catalog(&next_catalog).await.is_err() {
                        return Ok(Err(WorkflowControlRejection::Persistence));
                    }
                    match control.load(definition) {
                        Ok(()) => {
                            *catalog.borrow_mut() = next_catalog;
                            Ok(Ok(()))
                        }
                        Err(_error) => Ok(Err(WorkflowControlRejection::DuplicateId)),
                    }
                }
            },
        )?;

        let unload_control = self.runtime.control();
        let unload_catalog = Rc::clone(&self.catalog);
        context.register_rpc::<WorkflowUnload<M>, _>("system", move |_context, frames| {
            let control = unload_control.clone();
            let catalog = Rc::clone(&unload_catalog);
            async move {
                let request = match WorkflowJsonRequest::accept(frames).await? {
                    Ok(request) => request,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                let workflow_id = match request.workflow_id() {
                    Ok(workflow_id) => workflow_id,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                if !control.contains(&workflow_id) {
                    return Ok(Err(WorkflowControlRejection::NotFound));
                }

                let mut next_catalog = catalog.borrow().clone();
                let Some(position) = next_catalog
                    .iter()
                    .position(|workflow| workflow.id == workflow_id)
                else {
                    return Ok(Err(WorkflowControlRejection::Persistence));
                };
                next_catalog.remove(position);
                if write_catalog(&next_catalog).await.is_err() {
                    return Ok(Err(WorkflowControlRejection::Persistence));
                }
                if control.unload(&workflow_id).is_err() {
                    return Ok(Err(WorkflowControlRejection::NotFound));
                }
                *catalog.borrow_mut() = next_catalog;
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        self.runtime.start(context.rpc().clone());
        Box::pin(async move {
            (&mut self.runtime).await;
            Ok(())
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.runtime.stop();
        Ok(())
    }
}

async fn restore(
    control: &WorkflowRuntimeControl,
) -> Result<Vec<PersistedWorkflow>, EventRouterCreateError> {
    let bytes = match read(WORKFLOW_CATALOG_PATH).await {
        Ok(bytes) => bytes,
        Err(FsError::NotFound) => {
            write(WORKFLOW_CATALOG_PATH, b"[]").await?;
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let documents: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
        .map_err(|_error| invalid_catalog(WorkflowControlRejection::InvalidJson))?;
    let mut catalog = Vec::with_capacity(documents.len());
    for document in documents {
        let json = serde_json::to_vec(&document)
            .map_err(|_error| invalid_catalog(WorkflowControlRejection::InvalidJson))?;
        let request = WorkflowJsonRequest::try_from(json.clone()).map_err(invalid_catalog)?;
        let definition = request.definition().map_err(invalid_catalog)?;
        let id = definition.id().clone();
        control
            .load(definition)
            .map_err(|_error| invalid_catalog(WorkflowControlRejection::DuplicateId))?;
        catalog.push(PersistedWorkflow { id, json });
    }
    Ok(catalog)
}

async fn write_catalog(catalog: &[PersistedWorkflow]) -> Result<(), FsError> {
    let bytes = catalog_json(catalog);
    if let Err(error) = write(WORKFLOW_CATALOG_TEMP_PATH, &bytes).await {
        let _ignored = remove_file(WORKFLOW_CATALOG_TEMP_PATH).await;
        return Err(error);
    }
    if let Err(error) = rename(WORKFLOW_CATALOG_TEMP_PATH, WORKFLOW_CATALOG_PATH).await {
        let _ignored = remove_file(WORKFLOW_CATALOG_TEMP_PATH).await;
        return Err(error);
    }
    Ok(())
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

fn invalid_catalog(rejection: WorkflowControlRejection) -> EventRouterCreateError {
    EventRouterCreateError::InvalidPersistedWorkflow {
        path: String::from(WORKFLOW_CATALOG_PATH),
        rejection,
    }
}
