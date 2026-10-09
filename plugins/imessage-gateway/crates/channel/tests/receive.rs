//! Receive-loop scaffold driven by a mock slot pool and a scripted channel.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{pending, Future};
use std::rc::Rc;

use barracuda_imessage_gateway_channel::{
    receive_runtime, NoSlot, ReceiveChannel, ReceiveControl, ReceiveError, ReceiveFuture,
    ReceiveSession, ReceiveSlotSource, ReceiveState, ReceiveTiming,
};
use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use futures_lite::future::block_on;

const TIMING: ReceiveTiming = ReceiveTiming {
    initial_backoff: Duration::from_millis(20),
    max_backoff: Duration::from_millis(80),
    slot_retry: Duration::from_millis(20),
};

/// A fixed-size slot pool counting leases in use.
#[derive(Clone)]
struct Pool {
    capacity: usize,
    in_use: Rc<Cell<usize>>,
}

struct Lease(Rc<Cell<usize>>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl Pool {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            in_use: Rc::new(Cell::new(0)),
        }
    }

    fn in_use(&self) -> usize {
        self.in_use.get()
    }
}

impl ReceiveSlotSource for Pool {
    type Lease = Lease;

    fn acquire(&self) -> Option<Lease> {
        (self.in_use.get() < self.capacity).then(|| {
            self.in_use.set(self.in_use.get() + 1);
            Lease(Rc::clone(&self.in_use))
        })
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}

/// What one scripted session does.
enum Step {
    /// Fails before becoming healthy.
    Fail(ReceiveError),
    /// Becomes healthy, then fails.
    HealthyThenFail,
    /// Becomes healthy, then the server ends the session cleanly.
    HealthyThenEnd,
    /// Becomes healthy and runs until dropped.
    Forever,
}

/// Counts live sessions; a dropped session future drops its guard.
struct Live(Rc<Cell<usize>>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[derive(Default)]
struct Script {
    steps: RefCell<VecDeque<Step>>,
    started: RefCell<Vec<Instant>>,
    live: Rc<Cell<usize>>,
}

impl Script {
    fn new(steps: impl IntoIterator<Item = Step>) -> Rc<Self> {
        Rc::new(Self {
            steps: RefCell::new(steps.into_iter().collect()),
            ..Self::default()
        })
    }

    fn started(&self) -> Vec<Instant> {
        self.started.borrow().clone()
    }

    fn gaps(&self) -> Vec<u64> {
        self.started()
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).as_millis())
            .collect()
    }
}

impl ReceiveChannel<Lease> for Script {
    fn receive<'a>(
        &'a self,
        _lease: &'a mut Lease,
        session: ReceiveSession<'a>,
    ) -> ReceiveFuture<'a> {
        self.started.borrow_mut().push(Instant::now());
        self.live.set(self.live.get() + 1);
        let live = Live(Rc::clone(&self.live));
        let step = self.steps.borrow_mut().pop_front().unwrap_or(Step::Forever);
        Box::pin(async move {
            let _live = live;
            match step {
                Step::Fail(error) => Err(error),
                Step::HealthyThenFail => {
                    session.receiving();
                    Timer::after_millis(1).await;
                    Err(ReceiveError::new("dropped"))
                }
                Step::HealthyThenEnd => {
                    session.receiving();
                    Ok(())
                }
                Step::Forever => {
                    session.receiving();
                    pending().await
                }
            }
        })
    }
}

fn fail(message: &str) -> Step {
    Step::Fail(ReceiveError::new(message))
}

/// Runs `test` beside the receive runtime, as the Plugin task would.
fn scenario<Fut: Future<Output = ()>>(
    pool: Pool,
    script: Rc<Script>,
    test: impl FnOnce(Rc<ReceiveControl<Pool>>) -> Fut,
) {
    let control = Rc::new(ReceiveControl::new(pool));
    let runtime = receive_runtime(Rc::clone(&control), script, TIMING);
    block_on(async move {
        match select(runtime, test(control)).await {
            Either::First(()) => panic!("receive runtime ended"),
            Either::Second(()) => {}
        }
    });
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        Timer::after_millis(1).await;
    }
}

#[test]
fn idles_without_a_slot_until_enabled() {
    let pool = Pool::new(2);
    let script = Script::new([]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        Timer::after_millis(50).await;
        assert_eq!(control.state(), ReceiveState::Idle);
        assert!(script.started().is_empty());
        assert_eq!(pool.in_use(), 0);
    });
}

#[test]
fn starts_on_enable_and_frees_the_slot_on_disable() {
    let pool = Pool::new(2);
    let script = Script::new([Step::Forever]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        assert_eq!(control.set_enabled(true), Ok(()));
        assert_eq!(pool.in_use(), 1, "the slot is taken at once");
        assert_eq!(control.state(), ReceiveState::Starting);
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        assert_eq!(script.live.get(), 1);

        // Enabling again keeps the running session and its slot.
        assert_eq!(control.set_enabled(true), Ok(()));
        Timer::after_millis(10).await;
        assert_eq!(script.started().len(), 1);
        assert_eq!(pool.in_use(), 1);

        assert_eq!(control.set_enabled(false), Ok(()));
        assert_eq!(control.state(), ReceiveState::Idle);
        wait_until("released", || pool.in_use() == 0 && script.live.get() == 0).await;
        Timer::after_millis(30).await;
        assert_eq!(script.started().len(), 1, "no session while disabled");
        assert_eq!(control.state(), ReceiveState::Idle);

        assert_eq!(control.set_enabled(true), Ok(()));
        wait_until("receiving again", || {
            control.state() == ReceiveState::Receiving
        })
        .await;
        assert_eq!(script.started().len(), 2);
    });
}

#[test]
fn backs_off_only_on_errors_and_resets_after_success() {
    let pool = Pool::new(1);
    let script = Script::new([
        fail("one"),
        fail("two"),
        fail("three"),
        fail("four"),
        Step::HealthyThenFail,
        fail("five"),
        Step::HealthyThenEnd,
        Step::Forever,
    ]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        control.set_enabled(true).expect("slot");
        wait_until("first error", || control.state().name() == "error").await;
        assert_eq!(control.state(), ReceiveState::Error("one".into()));
        wait_until("eight sessions", || script.started().len() == 8).await;
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        let gaps = script.gaps();
        // 20 → 40 → 80 → capped at 80, then reset after the healthy session.
        let expected = [20, 40, 80, 80, 20, 40, 0];
        for (index, (gap, want)) in gaps.iter().zip(expected).enumerate() {
            assert!(
                *gap >= want,
                "gap {index} was {gap} ms, expected ≥ {want} ms: {gaps:?}"
            );
            assert!(
                *gap < want + 60,
                "gap {index} was {gap} ms, expected ≈ {want} ms: {gaps:?}"
            );
        }
        assert_eq!(pool.in_use(), 1, "the slot is held across reconnects");
    });
}

#[test]
fn retry_after_extends_the_backoff() {
    let pool = Pool::new(1);
    let script = Script::new([Step::Fail(
        ReceiveError::new("slow down").retry_after(Duration::from_millis(120)),
    )]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        control.set_enabled(true).expect("slot");
        wait_until("second session", || script.started().len() == 2).await;
        assert!(script.gaps()[0] >= 120, "{:?}", script.gaps());
    });
}

#[test]
fn reports_no_slot_and_takes_one_when_freed() {
    let pool = Pool::new(1);
    let script = Script::new([Step::Forever]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        let other = pool.acquire().expect("another channel's slot");
        assert_eq!(control.set_enabled(true), Err(NoSlot));
        assert_eq!(control.state(), ReceiveState::NoSlot);
        assert_eq!(control.capacity(), 1);
        Timer::after_millis(50).await;
        assert_eq!(
            control.state(),
            ReceiveState::NoSlot,
            "retries keep failing"
        );
        assert!(script.started().is_empty());

        drop(other);
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        assert_eq!(pool.in_use(), 1);
    });
}

#[test]
fn a_halted_loop_waits_for_restart() {
    let pool = Pool::new(1);
    let script = Script::new([
        Step::Fail(ReceiveError::halt("需要重新扫码")),
        Step::Forever,
    ]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        control.set_enabled(true).expect("slot");
        wait_until("halted", || control.state().name() == "error").await;
        assert_eq!(control.state().message(), Some("需要重新扫码"));
        Timer::after_millis(100).await;
        assert_eq!(script.started().len(), 1, "a halted loop does not retry");
        assert_eq!(pool.in_use(), 1);

        control.restart();
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        assert_eq!(script.started().len(), 2);
    });
}

#[test]
fn restart_replaces_the_running_session_and_keeps_the_slot() {
    let pool = Pool::new(1);
    let script = Script::new([Step::Forever, Step::Forever]);
    scenario(pool.clone(), Rc::clone(&script), |control| async move {
        control.set_enabled(true).expect("slot");
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        control.restart();
        wait_until("second session", || script.started().len() == 2).await;
        assert!(script.gaps()[0] < 20, "restart reconnects at once");
        wait_until("one live session", || script.live.get() == 1).await;
        assert_eq!(pool.in_use(), 1);
    });
}

#[test]
fn cancellation_stops_cleanly_and_frees_the_slot() {
    let pool = Pool::new(1);
    let script = Script::new([Step::Forever]);
    let control = Rc::new(ReceiveControl::new(pool.clone()));
    let runtime = receive_runtime(Rc::clone(&control), Rc::clone(&script), TIMING);
    // Stands in for `PluginTaskToken::cancelled`.
    let cancel: Signal<NoopRawMutex, ()> = Signal::new();
    let task = select(cancel.wait(), runtime);
    let test = async {
        control.set_enabled(true).expect("slot");
        wait_until("receiving", || control.state() == ReceiveState::Receiving).await;
        cancel.signal(());
    };
    let (outcome, ()) = block_on(join(task, test));
    assert!(
        matches!(outcome, Either::First(())),
        "cancellation ended the task"
    );
    assert_eq!(pool.in_use(), 0);
    assert_eq!(script.live.get(), 0);
    assert_eq!(control.state(), ReceiveState::Idle);

    // A stopped loop never takes a slot again.
    control.set_enabled(false).expect("disable");
    assert_eq!(control.set_enabled(true), Ok(()));
    assert_eq!(pool.in_use(), 0);
}
