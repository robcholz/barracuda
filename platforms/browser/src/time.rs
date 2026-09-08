//! Embassy time driver backed by the dedicated worker's monotonic clock.

use core::{cell::RefCell, task::Waker};

use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;

use crate::ffi;

#[derive(Debug)]
struct AlarmState {
    token: Option<i32>,
}

impl AlarmState {
    const fn new() -> Self {
        Self { token: None }
    }
}

struct Inner {
    alarm: AlarmState,
    last_micros: u64,
    queue: Queue,
}

impl Inner {
    const fn new() -> Self {
        Self {
            alarm: AlarmState::new(),
            last_micros: 0,
            queue: Queue::new(),
        }
    }

    fn now(&mut self) -> u64 {
        self.last_micros = self.last_micros.max(ffi::now_micros());
        self.last_micros
    }

    fn set_alarm(&mut self, timestamp: u64) -> bool {
        if let Some(token) = self.alarm.token.take() {
            ffi::cancel_alarm(token);
        }

        let now = self.now();
        if timestamp <= now {
            return false;
        }

        let delay_millis = timestamp
            .saturating_sub(now)
            .div_ceil(1_000)
            .min(u32::MAX as u64) as u32;
        let Ok(token) = ffi::set_alarm(delay_millis) else {
            return false;
        };
        self.alarm.token = Some(token);
        true
    }
}

thread_local! {
    static INNER: RefCell<Inner> = const { RefCell::new(Inner::new()) };
}

struct BrowserTimeDriver;

embassy_time_driver::time_driver_impl!(static DRIVER: BrowserTimeDriver = BrowserTimeDriver);

impl Driver for BrowserTimeDriver {
    fn now(&self) -> u64 {
        INNER.with(|inner| inner.borrow_mut().now())
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        INNER.with(|inner| {
            let mut inner = inner.borrow_mut();
            if inner.queue.schedule_wake(at, waker) {
                let now = inner.now();
                let mut next = inner.queue.next_expiration(now);
                while !inner.set_alarm(next) {
                    let now = inner.now();
                    next = inner.queue.next_expiration(now);
                }
            }
        });
    }
}

#[doc(hidden)]
pub fn dispatch() {
    INNER.with(|inner| {
        let mut inner = inner.borrow_mut();
        let now = inner.now();
        let mut next = inner.queue.next_expiration(now);
        while !inner.set_alarm(next) {
            let now = inner.now();
            next = inner.queue.next_expiration(now);
        }
    });
}
