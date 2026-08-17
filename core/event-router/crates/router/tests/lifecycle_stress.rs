#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use core::cell::Cell;
use core::task::Poll;
use std::rc::Rc;
use std::time::Instant;

use barracuda_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, Router, RunContext,
    UnregisterContext,
};
use barracuda_rpc::RpcLaneStorage;
use futures_lite::future::{block_on, poll_once};

const COMPONENTS: usize = 1_000;
const FRAME_CAPACITY: usize = 16;

#[derive(Default)]
struct Counts {
    registered: Cell<usize>,
    polled: Cell<usize>,
    unregistered: Cell<usize>,
}

struct PendingComponent {
    counts: Rc<Counts>,
}

impl Component<FRAME_CAPACITY> for PendingComponent {
    fn register(
        &mut self,
        _context: &mut RegisterContext<'_, FRAME_CAPACITY>,
    ) -> ComponentResult<()> {
        self.counts
            .registered
            .set(self.counts.registered.get().saturating_add(1));
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_CAPACITY>) -> ComponentFuture<'a> {
        Box::pin(core::future::poll_fn(move |_context| {
            self.counts
                .polled
                .set(self.counts.polled.get().saturating_add(1));
            Poll::Pending
        }))
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.counts
            .unregistered
            .set(self.counts.unregistered.get().saturating_add(1));
        Ok(())
    }
}

#[test]
fn many_components_load_poll_and_unload_without_lifecycle_leaks() {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<1, FRAME_CAPACITY, 0>::new()));
    let mut router = Router::new(lanes);
    let counts = Rc::new(Counts::default());
    let started = Instant::now();
    let mut ids = Vec::with_capacity(COMPONENTS);

    for _ in 0..COMPONENTS {
        ids.push(
            router
                .load(Box::new(PendingComponent {
                    counts: Rc::clone(&counts),
                }))
                .expect("load pending Component"),
        );
    }
    assert_eq!(counts.registered.get(), COMPONENTS);
    assert_eq!(ids.first().expect("first Component ID").value(), 1);
    assert_eq!(
        ids.last().expect("last Component ID").value(),
        COMPONENTS as u64
    );

    assert!(block_on(poll_once(&mut router)).is_none());
    assert_eq!(counts.polled.get(), COMPONENTS);

    for id in ids.into_iter().rev() {
        router.unload(id).expect("unload pending Component");
    }
    assert_eq!(counts.unregistered.get(), COMPONENTS);

    let elapsed = started.elapsed();
    eprintln!(
        "router_lifecycle: components={COMPONENTS}, elapsed_ns={}, components_per_second={:.0}",
        elapsed.as_nanos(),
        COMPONENTS as f64 / elapsed.as_secs_f64()
    );
}
