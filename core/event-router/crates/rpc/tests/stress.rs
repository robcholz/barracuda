#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use core::cell::Cell;
use core::task::Poll;
use std::rc::Rc;
use std::time::Instant;

use barracuda_rpc::{RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry, Unary};
use futures_lite::future::{block_on, poll_once};
use futures_util::future::join_all;

struct Echo;

impl RpcMethod for Echo {
    const ADDRESS: &'static str = "stress.echo";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

#[test]
fn one_lane_is_reused_across_many_sequential_calls() {
    const CALLS: u64 = 20_000;

    let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 8, 1>::new()));
    let registry = RpcRegistry::new(lanes);
    registry
        .register::<Echo, _>(
            "system",
            |_context, request: RpcFrame<[u8; 8]>| async move { Ok(Ok(*request.view()?)) },
        )
        .expect("register echo endpoint");
    let client = registry.client();
    let started = Instant::now();

    block_on(async {
        for value in 0..CALLS {
            let response = client
                .call::<Echo>(value.to_le_bytes())
                .expect("prepare call")
                .await
                .expect("finish call")
                .expect("method success");
            assert_eq!(response.view(), Ok(&value.to_le_bytes()));
            drop(response);
        }
    });

    let elapsed = started.elapsed();
    eprintln!(
        "rpc_sequential: calls={CALLS}, elapsed_ns={}, calls_per_second={:.0}",
        elapsed.as_nanos(),
        CALLS as f64 / elapsed.as_secs_f64()
    );
}

#[test]
fn exact_active_plus_waiter_window_drains_repeatedly() {
    const LANES: usize = 4;
    const WAITERS: usize = 8;
    const WINDOW: usize = LANES + WAITERS;
    const ROUNDS: u64 = 500;

    let lanes = Box::leak(Box::new(RpcLaneStorage::<LANES, 8, WAITERS>::new()));
    let registry = RpcRegistry::new(lanes);
    let gate = Rc::new(Cell::new(false));
    let handler_gate = Rc::clone(&gate);
    registry
        .register::<Echo, _>("system", move |_context, request: RpcFrame<[u8; 8]>| {
            let gate = Rc::clone(&handler_gate);
            async move {
                core::future::poll_fn(|_context| {
                    if gate.get() {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                Ok(Ok(*request.view()?))
            }
        })
        .expect("register gated echo endpoint");
    let client = registry.client();
    let started = Instant::now();

    block_on(async {
        for round in 0..ROUNDS {
            gate.set(false);
            let calls = (0..WINDOW).map(|offset| {
                let value = round
                    .saturating_mul(WINDOW as u64)
                    .saturating_add(offset as u64);
                let call = client
                    .call::<Echo>(value.to_le_bytes())
                    .expect("prepare windowed call");
                async move {
                    let response = call
                        .await
                        .expect("finish windowed call")
                        .expect("method success");
                    let copied = *response.view().expect("valid response frame");
                    drop(response);
                    copied
                }
            });
            let mut batch = Box::pin(join_all(calls));
            assert!(poll_once(batch.as_mut()).await.is_none());
            gate.set(true);
            let responses = batch.await;
            for (offset, response) in responses.into_iter().enumerate() {
                let expected = round
                    .saturating_mul(WINDOW as u64)
                    .saturating_add(offset as u64);
                assert_eq!(response, expected.to_le_bytes());
            }
        }
    });

    let calls = ROUNDS.saturating_mul(WINDOW as u64);
    let elapsed = started.elapsed();
    eprintln!(
        "rpc_windowed: calls={calls}, lanes={LANES}, waiters={WAITERS}, elapsed_ns={}, calls_per_second={:.0}",
        elapsed.as_nanos(),
        calls as f64 / elapsed.as_secs_f64()
    );
}
