use core::future::Future;

use super::ClawExecutor;

/// Host executor backed by a current-thread tokio runtime.
///
/// The orchestrator's worker calls `block_on` exactly once and stays parked in
/// it for the whole session lifetime, so building a runtime here is not a hot
/// path. `enable_all` turns on the time + IO drivers that async `reqwest` and
/// `TokioTimer` poll against. A current-thread runtime's `block_on` accepts the
/// `!Send` engine future.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioExecutor;

impl ClawExecutor for TokioExecutor {
    fn block_on<Fut: Future>(future: Fut) -> Fut::Output {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread tokio runtime for the orchestrator engine");
        runtime.block_on(future)
    }
}
