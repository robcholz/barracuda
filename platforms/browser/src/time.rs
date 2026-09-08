//! Embassy time driver backed by the dedicated worker's monotonic clock.

use core::{cell::RefCell, task::Waker};

use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use wasm_bindgen::{closure::Closure, JsCast as _};

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
    closure: Option<Closure<dyn FnMut()>>,
    last_micros: u64,
    queue: Queue,
    zero_millis: Option<f64>,
}

impl Inner {
    const fn new() -> Self {
        Self {
            alarm: AlarmState::new(),
            closure: None,
            last_micros: 0,
            queue: Queue::new(),
            zero_millis: None,
        }
    }

    fn now(&mut self) -> u64 {
        let current = monotonic_millis();
        let zero = *self.zero_millis.get_or_insert(current);
        let current_micros = ((current - zero).max(0.0) * 1_000.0) as u64;
        self.last_micros = self.last_micros.max(current_micros);
        self.last_micros
    }

    fn set_alarm(&mut self, timestamp: u64) -> bool {
        let worker = worker();
        if let Some(token) = self.alarm.token.take() {
            worker.clear_timeout_with_handle(token);
        }

        let now = self.now();
        if timestamp <= now {
            return false;
        }

        let delay_millis = timestamp
            .saturating_sub(now)
            .div_ceil(1_000)
            .min(i32::MAX as u64) as i32;
        let closure = self
            .closure
            .get_or_insert_with(|| Closure::wrap(Box::new(dispatch) as Box<dyn FnMut()>));
        let Ok(token) = worker.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            delay_millis,
        ) else {
            wasm_bindgen::throw_str("Browser Platform could not schedule an Embassy timer")
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

fn dispatch() {
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

fn monotonic_millis() -> f64 {
    worker()
        .performance()
        .map_or_else(js_sys::Date::now, |performance| performance.now())
}

fn worker() -> web_sys::DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>()
}
