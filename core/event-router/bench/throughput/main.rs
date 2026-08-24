//! Event Router throughput benchmark workload.
//!
//! This benchmark emits CSV to stdout. The supported throughput suites are
//! orchestrated by `core/event-router/bench/throughput/run.py`.

#![allow(clippy::expect_used)]
#![allow(missing_docs)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::too_many_arguments)]

use core::cell::{Cell, RefCell};
use core::future::{pending, Future};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::rc::Rc;
use std::time::{Duration, Instant};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, Event, EventEmitter, EventRouter, RegisterContext,
    RpcFrame, RpcLaneStorage, RpcMethod, RunContext, Unary, UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use futures_lite::future::{block_on, yield_now};

const DEFAULT_SAMPLES: usize = 7;

struct BenchEvent<const P: usize>;

impl<const P: usize> Event for BenchEvent<P> {
    const ID: &'static str = "bench.event";
    type Message = [u8; P];
    type Input = Unary;
}

struct Sink<const P: usize>;

impl<const P: usize> RpcMethod for Sink<P> {
    const ADDRESS: &'static str = "bench.sink";
    type Request = [u8; P];
    type Response = ();
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[derive(Default)]
struct State {
    done: Cell<bool>,
    received: Cell<usize>,
    checksum: Cell<u64>,
    elapsed: Cell<Option<Duration>>,
    error: RefCell<Option<String>>,
}

struct Producer<const N: usize, const M: usize, const P: usize> {
    events: usize,
    warmup: usize,
    fanout: usize,
    sink_yields: usize,
    matched: bool,
    state: Rc<State>,
}

impl<const N: usize, const M: usize, const P: usize> Component<M> for Producer<N, M, P> {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        if !self.matched {
            return Ok(());
        }
        let state = Rc::clone(&self.state);
        let sink_yields = self.sink_yields;
        context.register_rpc::<Sink<P>, _>(move |_context, request: RpcFrame<[u8; P]>| {
            let state = Rc::clone(&state);
            async move {
                let bytes = request.view()?;
                let edge = u64::from(bytes.first().copied().unwrap_or(0))
                    .wrapping_add(u64::from(bytes.last().copied().unwrap_or(0)));
                for _ in 0..sink_yields {
                    yield_now().await;
                }
                state.checksum.set(state.checksum.get().wrapping_add(edge));
                state.received.set(state.received.get().saturating_add(1));
                Ok(Ok(()))
            }
        })
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            if self.matched {
                let workflow = WorkflowClient::<M>::new(context.rpc().clone());
                for index in 0..self.fanout {
                    let json = format!(
                        r#"{{"id":"bench-{index}","match":{{"event":"bench.event"}},"steps":[{{"call":"bench.sink"}}]}}"#
                    );
                    if let Err(error) = workflow.load(&json).await {
                        self.fail(error.to_string()).await;
                    }
                }
            }

            let emitter = EventEmitter::<M>::new(context.rpc().clone());
            let payload = [0x5a; P];
            let in_flight_limit = if self.matched {
                (N.saturating_sub(1) / self.fanout).max(1)
            } else {
                1
            };
            if let Err(error) = emit_phase(
                &emitter,
                &payload,
                self.warmup,
                self.fanout,
                self.matched,
                in_flight_limit,
                &self.state,
                0,
            )
            .await
            {
                self.fail(error).await;
            }
            let warmup_received = self.warmup.saturating_mul(self.fanout);
            while self.matched && self.state.received.get() < warmup_received {
                yield_now().await;
            }

            let started = Instant::now();
            if let Err(error) = emit_phase(
                &emitter,
                &payload,
                self.events,
                self.fanout,
                self.matched,
                in_flight_limit,
                &self.state,
                self.warmup,
            )
            .await
            {
                self.fail(error).await;
            }
            let target = self
                .warmup
                .saturating_add(self.events)
                .saturating_mul(self.fanout);
            while self.matched && self.state.received.get() < target {
                yield_now().await;
            }
            self.state.elapsed.set(Some(started.elapsed()));
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

impl<const N: usize, const M: usize, const P: usize> Producer<N, M, P> {
    async fn fail(&self, error: String) -> ! {
        self.state.error.replace(Some(error));
        self.state.done.set(true);
        pending().await
    }
}

async fn emit_phase<const M: usize, const P: usize>(
    emitter: &EventEmitter<M>,
    payload: &[u8; P],
    events: usize,
    fanout: usize,
    matched: bool,
    in_flight_limit: usize,
    state: &State,
    already_emitted: usize,
) -> Result<(), String> {
    for offset in 0..events {
        if matched {
            let emitted = already_emitted.saturating_add(offset);
            loop {
                let completed = state.received.get() / fanout;
                if emitted.saturating_sub(completed) < in_flight_limit {
                    break;
                }
                yield_now().await;
            }
        }
        emitter
            .emit::<BenchEvent<P>>(*payload)
            .await
            .map_err(|error| error.to_string())?;
        if matched {
            yield_now().await;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ResultRow {
    scenario: &'static str,
    n: usize,
    m: usize,
    q: usize,
    payload: usize,
    fanout: usize,
    sink_yields: usize,
    events: usize,
    sample: usize,
    elapsed_ns: u128,
    ingress_bytes_per_second: f64,
    routed_bytes_per_second: f64,
    events_per_second: f64,
    checksum: u64,
}

fn drive_until_done<const N: usize, const M: usize, const Q: usize>(
    router: &mut EventRouter<N, M, Q>,
    state: &State,
) -> Result<(), String> {
    const POLL_LIMIT: usize = 20_000_000;
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..POLL_LIMIT {
        match Pin::new(&mut *router).poll(&mut context) {
            Poll::Ready(Ok(())) => return Err(String::from("router completed unexpectedly")),
            Poll::Ready(Err(error)) => return Err(error.to_string()),
            Poll::Pending => {}
        }
        if state.done.get() {
            return match state.error.borrow().clone() {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
    }
    Err(String::from("poll limit exceeded"))
}

#[allow(clippy::too_many_arguments)]
fn one_sample<const N: usize, const M: usize, const Q: usize, const P: usize>(
    scenario: &'static str,
    matched: bool,
    fanout: usize,
    sink_yields: usize,
    events: usize,
    warmup: usize,
    sample: usize,
) -> Result<ResultRow, String> {
    if matched && N <= fanout {
        return Err(format!("N={N} must exceed fanout={fanout}"));
    }
    let lanes = Box::leak(Box::new(RpcLaneStorage::<N, M, Q>::new()));
    block_on(install_global_memory_vfs()).map_err(|error| error.to_string())?;
    let mut router = block_on(EventRouter::new(lanes)).map_err(|error| error.to_string())?;
    let state = Rc::new(State::default());
    router
        .load(Box::new(Producer::<N, M, P> {
            events,
            warmup,
            fanout,
            sink_yields,
            matched,
            state: Rc::clone(&state),
        }))
        .map_err(|error| error.to_string())?;
    drive_until_done(&mut router, &state)?;

    let elapsed = state
        .elapsed
        .get()
        .ok_or_else(|| String::from("missing elapsed time"))?;
    let seconds = elapsed.as_secs_f64();
    let ingress_bytes = events.saturating_mul(P);
    let routed_bytes = ingress_bytes.saturating_mul(if matched { fanout } else { 1 });
    Ok(ResultRow {
        scenario,
        n: N,
        m: M,
        q: Q,
        payload: P,
        fanout,
        sink_yields,
        events,
        sample,
        elapsed_ns: elapsed.as_nanos(),
        ingress_bytes_per_second: ingress_bytes as f64 / seconds,
        routed_bytes_per_second: routed_bytes as f64 / seconds,
        events_per_second: events as f64 / seconds,
        checksum: state.checksum.get(),
    })
}

fn print_header() {
    println!(
        "scenario,n,m,q,payload,fanout,sink_yields,events,sample,elapsed_ns,\
         ingress_bytes_per_second,routed_bytes_per_second,events_per_second,checksum"
    );
}

fn print_row(row: ResultRow) {
    println!(
        "{},{},{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{}",
        row.scenario,
        row.n,
        row.m,
        row.q,
        row.payload,
        row.fanout,
        row.sink_yields,
        row.events,
        row.sample,
        row.elapsed_ns,
        row.ingress_bytes_per_second,
        row.routed_bytes_per_second,
        row.events_per_second,
        row.checksum,
    );
}

macro_rules! samples {
    ($count:expr, $scenario:literal, $matched:expr, $fanout:expr, $yields:expr, $events:expr, $warmup:expr, $n:literal, $m:literal, $q:literal, $p:literal) => {{
        for sample in 0..$count {
            let row = one_sample::<$n, $m, $q, $p>(
                $scenario, $matched, $fanout, $yields, $events, $warmup, sample,
            )?;
            print_row(row);
        }
    }};
}

fn run_quick(count: usize) -> Result<(), String> {
    samples!(
        count,
        "quick_unmatched",
        false,
        1,
        0,
        8_000,
        500,
        1,
        64,
        0,
        8
    );
    samples!(count, "quick_small", true, 1, 0, 8_000, 500, 2, 64, 0, 8);
    samples!(
        count,
        "quick_medium",
        true,
        1,
        0,
        5_000,
        400,
        2,
        320,
        0,
        256
    );
    samples!(
        count,
        "quick_large",
        true,
        1,
        0,
        1_000,
        100,
        2,
        4122,
        0,
        4096
    );
    samples!(
        count,
        "quick_fanout",
        true,
        4,
        0,
        1_500,
        100,
        5,
        288,
        0,
        256
    );
    samples!(count, "quick_slow", true, 1, 32, 1_500, 100, 8, 288, 0, 256);
    Ok(())
}

macro_rules! for_matrix_unmatched {
    ($count:expr) => {{
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            64,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            128,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            256,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            512,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            1024,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            2048,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            30_000,
            2_000,
            1,
            4096,
            0,
            8
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            12_000,
            1_000,
            1,
            256,
            0,
            256
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            12_000,
            1_000,
            1,
            512,
            0,
            256
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            12_000,
            1_000,
            1,
            1024,
            0,
            256
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            12_000,
            1_000,
            1,
            2048,
            0,
            256
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            12_000,
            1_000,
            1,
            4096,
            0,
            256
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            4_000,
            400,
            1,
            4096,
            0,
            4096
        );
        samples!(
            $count,
            "unmatched_m",
            false,
            1,
            0,
            4_000,
            400,
            1,
            8192,
            0,
            4096
        );
    }};
}

macro_rules! for_matrix_matched {
    ($count:expr) => {{
        samples!($count, "matched_m", true, 1, 0, 20_000, 1_500, 2, 64, 0, 8);
        samples!($count, "matched_m", true, 1, 0, 20_000, 1_500, 2, 128, 0, 8);
        samples!($count, "matched_m", true, 1, 0, 20_000, 1_500, 2, 256, 0, 8);
        samples!($count, "matched_m", true, 1, 0, 20_000, 1_500, 2, 512, 0, 8);
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            20_000,
            1_500,
            2,
            1024,
            0,
            8
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            20_000,
            1_500,
            2,
            2048,
            0,
            8
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            20_000,
            1_500,
            2,
            4096,
            0,
            8
        );
        samples!($count, "matched_m", true, 1, 0, 10_000, 800, 2, 256, 0, 256);
        samples!($count, "matched_m", true, 1, 0, 10_000, 800, 2, 512, 0, 256);
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            10_000,
            800,
            2,
            1024,
            0,
            256
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            10_000,
            800,
            2,
            2048,
            0,
            256
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            10_000,
            800,
            2,
            4096,
            0,
            256
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            2_500,
            250,
            2,
            4096,
            0,
            4096
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            2_500,
            250,
            2,
            8192,
            0,
            4096
        );
        samples!(
            $count,
            "matched_m",
            true,
            1,
            0,
            2_500,
            250,
            2,
            16384,
            0,
            4096
        );
    }};
}

macro_rules! for_matrix_n_q_fanout {
    ($count:expr) => {{
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 2, 512, 0, 256);
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 3, 512, 0, 256);
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 4, 512, 0, 256);
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 8, 512, 0, 256);
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 16, 512, 0, 256);
        samples!($count, "fast_n", true, 1, 0, 15_000, 1_000, 32, 512, 0, 256);
        samples!($count, "q_sweep", true, 1, 0, 15_000, 1_000, 2, 512, 0, 256);
        samples!($count, "q_sweep", true, 1, 0, 15_000, 1_000, 2, 512, 1, 256);
        samples!($count, "q_sweep", true, 1, 0, 15_000, 1_000, 2, 512, 4, 256);
        samples!($count, "q_sweep", true, 1, 0, 15_000, 1_000, 2, 512, 16, 256);
        samples!($count, "q_sweep", true, 1, 0, 15_000, 1_000, 2, 512, 64, 256);
        samples!($count, "fanout", true, 1, 0, 8_000, 600, 2, 512, 0, 256);
        samples!($count, "fanout", true, 1, 0, 8_000, 600, 3, 512, 0, 256);
        samples!($count, "fanout", true, 2, 0, 6_000, 500, 3, 512, 0, 256);
        samples!($count, "fanout", true, 2, 0, 6_000, 500, 5, 512, 0, 256);
        samples!($count, "fanout", true, 4, 0, 3_000, 300, 5, 512, 0, 256);
        samples!($count, "fanout", true, 4, 0, 3_000, 300, 9, 512, 0, 256);
        samples!($count, "fanout", true, 8, 0, 1_500, 150, 9, 512, 0, 256);
        samples!($count, "fanout", true, 8, 0, 1_500, 150, 17, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 8, 5_000, 300, 2, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 8, 5_000, 300, 4, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 8, 5_000, 300, 8, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 8, 5_000, 300, 16, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 8, 5_000, 300, 32, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 32, 2_500, 200, 2, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 32, 2_500, 200, 4, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 32, 2_500, 200, 8, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 32, 2_500, 200, 16, 512, 0, 256);
        samples!($count, "slow_n", true, 1, 32, 2_500, 200, 32, 512, 0, 256);
    }};
}

fn run_matrix(count: usize) -> Result<(), String> {
    for_matrix_unmatched!(count);
    for_matrix_matched!(count);
    for_matrix_n_q_fanout!(count);
    Ok(())
}

fn run_fine_m(count: usize) -> Result<(), String> {
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 40, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 48, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 56, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 64, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 80, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 96, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 112, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 40_000, 3_000, 2, 128, 0, 8);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 256, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 272, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 280, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 282, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 288, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 304, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 320, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 384, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 448, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 20_000, 1_500, 2, 512, 0, 256);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4096, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4112, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4120, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4122, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4128, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4160, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4224, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4352, 0, 4096);
    samples!(count, "fine_m", true, 1, 0, 5_000, 500, 2, 4608, 0, 4096);
    Ok(())
}

fn run_joint(count: usize) -> Result<(), String> {
    samples!(
        count,
        "joint_fast",
        true,
        1,
        0,
        25_000,
        2_000,
        2,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fast",
        true,
        1,
        0,
        25_000,
        2_000,
        3,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fast",
        true,
        1,
        0,
        25_000,
        2_000,
        4,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fast",
        true,
        1,
        0,
        25_000,
        2_000,
        8,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fast",
        true,
        1,
        0,
        25_000,
        2_000,
        16,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        2,
        0,
        10_000,
        800,
        3,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        2,
        0,
        10_000,
        800,
        5,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        4,
        0,
        6_000,
        500,
        5,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        4,
        0,
        6_000,
        500,
        9,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        8,
        0,
        3_000,
        300,
        9,
        288,
        0,
        256
    );
    samples!(
        count,
        "joint_fanout",
        true,
        8,
        0,
        3_000,
        300,
        17,
        288,
        0,
        256
    );
    samples!(count, "joint_slow", true, 1, 32, 5_000, 400, 2, 288, 0, 256);
    samples!(count, "joint_slow", true, 1, 32, 5_000, 400, 4, 288, 0, 256);
    samples!(count, "joint_slow", true, 1, 32, 5_000, 400, 8, 288, 0, 256);
    samples!(
        count,
        "joint_slow",
        true,
        1,
        32,
        5_000,
        400,
        16,
        288,
        0,
        256
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Suite {
    Quick,
    Matrix,
    FineM,
    Joint,
}

struct Arguments {
    suite: Suite,
    samples: usize,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut suite = None;
    let mut samples = DEFAULT_SAMPLES;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--suite requires a value"))?;
                suite = Some(match value.as_str() {
                    "quick" => Suite::Quick,
                    "matrix" => Suite::Matrix,
                    "fine-m" => Suite::FineM,
                    "joint" => Suite::Joint,
                    _ => return Err(format!("unknown suite: {value}")),
                });
            }
            "--samples" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--samples requires a value"))?;
                samples = value
                    .parse::<usize>()
                    .map_err(|error| format!("invalid sample count: {error}"))?;
                if samples == 0 {
                    return Err(String::from("sample count must be positive"));
                }
            }
            // Cargo appends this marker when launching a custom benchmark target.
            "--bench" => {}
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(Arguments {
        suite: suite.ok_or_else(|| String::from("--suite is required"))?,
        samples,
    })
}

fn main() -> Result<(), String> {
    let arguments = parse_arguments()?;
    print_header();
    match arguments.suite {
        Suite::Quick => run_quick(arguments.samples),
        Suite::Matrix => run_matrix(arguments.samples),
        Suite::FineM => run_fine_m(arguments.samples),
        Suite::Joint => run_joint(arguments.samples),
    }
}
