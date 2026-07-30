//! Host smoke test for the agent runtime's configured worker stack limit.
//!
//! Unlike the heap profiler, this binary does not install DHAT. Profiling
//! allocator backtraces would consume stack and distort this check.

use std::io;

use claw_agent::{AgentPersistenceConfig, AgentSystem};
use claw_interface::http::{
    Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpResponseFuture, HttpStatusCode, SliceChunks,
    StreamingHttp,
};
use claw_interface::{
    ClawThread, CoreAffinity, ImmediateTimer, MemFs, Priority, TokioExecutor, WorkerHandle,
};

type StackAgentSystem = AgentSystem<MemFs, NeverHttp, ImmediateTimer>;

#[derive(Default)]
struct NeverHttp;

impl ClawHttp for NeverHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async { panic!("agent-init-stack must not call HTTP") })
    }
}

impl StreamingHttp for NeverHttp {
    type ByteStream<'a>
        = SliceChunks<'a>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        _request: &'r HttpJsonRequest<'r>,
        _cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        panic!("agent-init-stack must not call streaming HTTP")
    }
}

/// Host thread policy that honors the stack size requested by `claw-core`.
struct BoundedThread;

impl ClawThread for BoundedThread {
    fn spawn_worker<F>(
        name: &str,
        stack_size: usize,
        priority: Priority,
        affinity: CoreAffinity,
        f: F,
    ) -> io::Result<WorkerHandle>
    where
        F: FnOnce() + Send + 'static,
    {
        let _ = (priority, affinity);
        println!("worker={name}");
        println!("worker_stack_limit_bytes={stack_size}");
        std::thread::Builder::new()
            .name(name.to_owned())
            .stack_size(stack_size)
            .spawn(f)
            .map(WorkerHandle::new)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = StackAgentSystem::new::<BoundedThread, TokioExecutor>(
        MemFs::new(),
        AgentPersistenceConfig {
            persistence_root: "/profile/agent-init-stack".to_owned(),
            skill_roots: Vec::new(),
        },
    )?;

    drop(system);
    println!("scenario=agent-init-stack");
    println!("status=passed");
    Ok(())
}
