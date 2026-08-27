use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use http_client::embedded_nal_async::{Dns, TcpConnect};

use barracuda_model_api::ModelApi;

/// Private shared lease helper used by the concrete memory-side LLM providers.
pub(super) struct SharedAsyncLlm<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    api: Mutex<NoopRawMutex, ModelApi<'static, Tcp, Resolver>>,
}

impl<Tcp, Resolver> SharedAsyncLlm<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(super) fn new(api: ModelApi<'static, Tcp, Resolver>) -> Self {
        Self {
            api: Mutex::new(api),
        }
    }

    pub(super) async fn lease(
        &self,
    ) -> MutexGuard<'_, NoopRawMutex, ModelApi<'static, Tcp, Resolver>> {
        self.api.lock().await
    }
}
