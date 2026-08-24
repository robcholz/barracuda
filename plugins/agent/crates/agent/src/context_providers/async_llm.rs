use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};

use barracuda_model_api::ModelApi;
use embedded_nal_async::{Dns, TcpConnect};

/// Private shared lease helper used by the concrete memory-side LLM providers.
pub(super) struct SharedAsyncLlm<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> {
    api: Mutex<NoopRawMutex, ModelApi<'static, Tcp, Resolver>>,
}

impl<Tcp: TcpConnect + 'static, Resolver: Dns + 'static> SharedAsyncLlm<Tcp, Resolver> {
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

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::Wake;

    use barracuda_model_api::ModelApi;
    use barracuda_platform_test::NeverStack;

    use super::SharedAsyncLlm;

    #[derive(Default)]
    struct ReadyFlag(AtomicBool);

    impl ReadyFlag {
        fn take(&self) -> bool {
            self.0.swap(false, Ordering::AcqRel)
        }
    }

    impl Wake for ReadyFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn poll_with<F: Future>(future: Pin<&mut F>, ready: &Arc<ReadyFlag>) -> Poll<F::Output> {
        let waker = Waker::from(Arc::clone(ready));
        future.poll(&mut Context::from_waker(&waker))
    }

    #[test]
    fn every_independent_lease_waiter_makes_progress() {
        static NETWORK: NeverStack = NeverStack;
        let api = ModelApi::new(&NETWORK, &NETWORK, 256, 256);
        let shared = SharedAsyncLlm::new(api);
        let holder = futures_lite::future::block_on(shared.lease());

        let ready = [
            Arc::new(ReadyFlag::default()),
            Arc::new(ReadyFlag::default()),
        ];
        let mut waiters = [Box::pin(shared.lease()), Box::pin(shared.lease())];
        for (waiter, ready) in waiters.iter_mut().zip(&ready) {
            assert!(poll_with(waiter.as_mut(), ready).is_pending());
        }

        drop(holder);

        let mut completed = [false; 2];
        for _ in 0..waiters.len() {
            let mut polled = false;
            for ((waiter, ready), completed) in waiters.iter_mut().zip(&ready).zip(&mut completed) {
                if !*completed && ready.take() {
                    polled = true;
                    if let Poll::Ready(lease) = poll_with(waiter.as_mut(), ready) {
                        *completed = true;
                        drop(lease);
                    }
                }
            }
            if completed.iter().all(|completed| *completed) {
                break;
            }
            if !polled {
                break;
            }
        }

        assert!(
            completed.into_iter().all(|completed| completed),
            "returning each lease must eventually wake every independent waiter"
        );
    }
}
