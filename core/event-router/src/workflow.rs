//! Durable Workflow Runtime adapter owned by Event Router.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_fs::{FileSystem, FsError};
use barracuda_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_rpc::RpcContext;
use barracuda_workflow::integration::{
    InternalEmit, WorkflowJsonRequest, WorkflowLoad, WorkflowRuntime, WorkflowRuntimeControl,
    WorkflowRuntimeView, WorkflowUnload,
};
use barracuda_workflow::{validate_definition, WorkflowControlRejection, WorkflowId};

use crate::EventRouterCreateError;

const WORKFLOW_INDEX_FILE: &str = "index";

pub(super) struct WorkflowComponent<Filesystem>
where
    Filesystem: FileSystem,
{
    runtime: WorkflowRuntime,
    filesystem: &'static Filesystem,
    directory: String,
    index: Rc<RefCell<Vec<u8>>>,
}

impl<Filesystem> WorkflowComponent<Filesystem>
where
    Filesystem: FileSystem,
{
    pub(super) fn new(
        filesystem: &'static Filesystem,
        directory: String,
    ) -> Result<(Self, WorkflowRuntimeView), EventRouterCreateError> {
        if directory.trim().is_empty() {
            return Err(EventRouterCreateError::InvalidPersistenceDirectory);
        }
        filesystem.create_dir_all(&directory)?;
        let runtime = WorkflowRuntime::new();
        let view = runtime.view();
        let index = restore(&runtime.control(), filesystem, &directory)?;
        Ok((
            Self {
                runtime,
                filesystem,
                directory,
                index: Rc::new(RefCell::new(index)),
            },
            view,
        ))
    }
}

impl<Filesystem, const M: usize> Component<M> for WorkflowComponent<Filesystem>
where
    Filesystem: FileSystem,
{
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<InternalEmit<M>, _>(self.runtime.ingress_handler::<M>())?;

        let load_control = self.runtime.control();
        let load_index = Rc::clone(&self.index);
        let load_filesystem = self.filesystem;
        let load_directory = self.directory.clone();
        context.register_rpc::<WorkflowLoad<M>, _>(move |context: RpcContext, frames| {
            let control = load_control.clone();
            let index = Rc::clone(&load_index);
            let directory = load_directory.clone();
            async move {
                let request = match WorkflowJsonRequest::accept(frames).await? {
                    Ok(request) => request,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                let definition = match request.definition() {
                    Ok(definition) => definition,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                // Primary gate: reject workflows whose steps cannot be
                // resolved and linked before anything is persisted.
                if let Err(rejection) = validate_definition(context.client(), &definition) {
                    return Ok(Err(rejection));
                }
                if control.contains(definition.id()) {
                    return Ok(Err(WorkflowControlRejection::DuplicateId));
                }

                let workflow_path = workflow_path(&directory, definition.id());
                if load_filesystem
                    .write_atomic(&workflow_path, request.bytes())
                    .is_err()
                {
                    return Ok(Err(WorkflowControlRejection::Persistence));
                }
                let mut index = index.borrow_mut();
                let original_len = index.len();
                index.extend_from_slice(definition.id().as_str().as_bytes());
                index.push(b'\n');
                if write_index(load_filesystem, &directory, &index).is_err() {
                    index.truncate(original_len);
                    return Ok(Err(WorkflowControlRejection::Persistence));
                }
                drop(index);
                match control.load(definition) {
                    Ok(()) => Ok(Ok(())),
                    Err(_error) => Ok(Err(WorkflowControlRejection::DuplicateId)),
                }
            }
        })?;

        let unload_control = self.runtime.control();
        let unload_index = Rc::clone(&self.index);
        let unload_filesystem = self.filesystem;
        let unload_directory = self.directory.clone();
        context.register_rpc::<WorkflowUnload<M>, _>(move |_context, frames| {
            let control = unload_control.clone();
            let index = Rc::clone(&unload_index);
            let directory = unload_directory.clone();
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
                let mut index = index.borrow_mut();
                let previous = index.clone();
                if !remove_index_entry(&mut index, &workflow_id)
                    || write_index(unload_filesystem, &directory, &index).is_err()
                {
                    *index = previous;
                    return Ok(Err(WorkflowControlRejection::Persistence));
                }
                drop(index);
                if control.unload(&workflow_id).is_err() {
                    return Ok(Err(WorkflowControlRejection::NotFound));
                }
                // The index is authoritative. A stale orphan is ignored on
                // restart and can be overwritten by a future load.
                let _ignored = unload_filesystem.remove(&workflow_path(&directory, &workflow_id));
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

fn restore<Filesystem>(
    control: &WorkflowRuntimeControl,
    filesystem: &Filesystem,
    directory: &str,
) -> Result<Vec<u8>, EventRouterCreateError>
where
    Filesystem: FileSystem,
{
    let index_path = index_path(directory);
    let index = match filesystem.read(&index_path) {
        Ok(index) => index,
        Err(FsError::NotFound) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let index_text = core::str::from_utf8(&index).map_err(|_error| {
        EventRouterCreateError::InvalidPersistedWorkflow {
            path: index_path.clone(),
            rejection: WorkflowControlRejection::InvalidWorkflowId,
        }
    })?;
    for id in index_text.lines() {
        let workflow_id = WorkflowId::try_from(id).map_err(|_error| {
            EventRouterCreateError::InvalidPersistedWorkflow {
                path: index_path.clone(),
                rejection: WorkflowControlRejection::InvalidWorkflowId,
            }
        })?;
        let path = workflow_path(directory, &workflow_id);
        let request =
            WorkflowJsonRequest::from_bytes(filesystem.read(&path)?).map_err(|rejection| {
                EventRouterCreateError::InvalidPersistedWorkflow {
                    path: path.clone(),
                    rejection,
                }
            })?;
        let definition = request.definition().map_err(|rejection| {
            EventRouterCreateError::InvalidPersistedWorkflow {
                path: path.clone(),
                rejection,
            }
        })?;
        if definition.id() != &workflow_id {
            return Err(EventRouterCreateError::MismatchedPersistedWorkflow(path));
        }
        control.load(definition).map_err(|_error| {
            EventRouterCreateError::InvalidPersistedWorkflow {
                path,
                rejection: WorkflowControlRejection::DuplicateId,
            }
        })?;
    }
    Ok(index)
}

fn write_index<Filesystem>(
    filesystem: &Filesystem,
    directory: &str,
    bytes: &[u8],
) -> Result<(), FsError>
where
    Filesystem: FileSystem,
{
    filesystem.write_atomic(&index_path(directory), bytes)
}

fn remove_index_entry(bytes: &mut Vec<u8>, workflow_id: &WorkflowId) -> bool {
    let needle = workflow_id.as_str().as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let Some(remaining) = bytes.get(start..) else {
            return false;
        };
        let end = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |offset| start.saturating_add(offset));
        if bytes.get(start..end) == Some(needle) {
            let remove_end = end.saturating_add(usize::from(end < bytes.len()));
            bytes.drain(start..remove_end);
            return true;
        }
        start = end.saturating_add(1);
    }
    false
}

fn index_path(directory: &str) -> String {
    join(directory, WORKFLOW_INDEX_FILE)
}

fn workflow_path(directory: &str, workflow_id: &WorkflowId) -> String {
    join(directory, &format!("{}.json", workflow_id.as_str()))
}

fn join(directory: &str, name: &str) -> String {
    format!("{}/{}", directory.trim_end_matches('/'), name)
}
