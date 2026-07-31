use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tokio::sync::broadcast;

const SNAPSHOT_CAPACITY: usize = 64;

static HUB: OnceLock<broadcast::Sender<String>> = OnceLock::new();
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static SERIALIZATION_FAILURES: AtomicU64 = AtomicU64::new(0);

fn sender() -> &'static broadcast::Sender<String> {
    HUB.get_or_init(|| {
        let (sender, receiver) = broadcast::channel(SNAPSHOT_CAPACITY);
        drop(receiver);
        sender
    })
}

pub(super) fn is_active() -> bool {
    HUB.get().is_some_and(|sender| sender.receiver_count() > 0)
}

pub(super) fn next_sequence() -> u64 {
    NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

pub(super) fn subscribe() -> broadcast::Receiver<String> {
    sender().subscribe()
}

pub(super) fn publish(payload: String) {
    drop(sender().send(payload));
}

pub(super) fn record_serialization_failure() {
    SERIALIZATION_FAILURES.fetch_add(1, Ordering::Relaxed);
}
