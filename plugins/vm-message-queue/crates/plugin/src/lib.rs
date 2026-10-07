//! Shared, bounded message queues exposed as a require-only Lua package.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_runtime_utils::{Cancel, CancellationFlag};
use barracuda_vm_plugin::{Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use portable_atomic::{AtomicBool, AtomicUsize, Ordering};
use portable_atomic_util::Arc;
use spin::Mutex;

/// Maximum number of distinct queue keys.
pub const MAX_KEYS: usize = 16;
/// Maximum UTF-8 byte length of a queue key.
pub const KEY_MAX_LEN: usize = 64;
/// Maximum byte length of one binary-safe message.
pub const MESSAGE_MAX_LEN: usize = 4096;
/// Maximum queued messages for each key.
pub const QUEUE_DEPTH: usize = 8;

/// Registers the shared `message_queue` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct MessageQueuePlugin;

impl MessageQueuePlugin {
    /// Creates the stateless Plugin entry; queue state is constructed with its package.
    #[must_use]
    pub fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for MessageQueuePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(MessageQueuePackage::new())
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

/// Queued payloads live in bulk memory; the channel itself holds only handles.
type Queue = Channel<CriticalSectionRawMutex, Bytes, QUEUE_DEPTH>;

struct MessageQueuePackage {
    state: Arc<SharedState>,
}

impl MessageQueuePackage {
    fn new() -> Self {
        Self {
            state: Arc::new(SharedState::new()),
        }
    }
}

struct SharedState {
    queues: Mutex<Vec<(String, Arc<Queue>)>>,
    lifecycle: Mutex<Lifecycle>,
    next_operation: AtomicUsize,
    active: AtomicBool,
}

struct Lifecycle {
    operations: Vec<(usize, Arc<CancellationFlag>)>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            queues: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(Lifecycle {
                operations: Vec::new(),
            }),
            next_operation: AtomicUsize::new(0),
            active: AtomicBool::new(true),
        }
    }

    fn queue(&self, key: String) -> Result<Arc<Queue>> {
        if key.is_empty() {
            return Err(Error::runtime("message queue key must not be empty"));
        }
        if key.len() > KEY_MAX_LEN {
            return Err(Error::runtime(format!(
                "message queue key exceeds {KEY_MAX_LEN} bytes"
            )));
        }
        if !self.active.load(Ordering::Acquire) {
            return Err(revoked_error());
        }

        let mut queues = self.queues.lock();
        if let Some((_, queue)) = queues.iter().find(|(existing, _)| existing == &key) {
            return Ok(Arc::clone(queue));
        }
        if queues.len() == MAX_KEYS {
            return Err(Error::runtime(format!(
                "message queue key limit of {MAX_KEYS} reached"
            )));
        }
        let queue = Arc::new(Channel::new());
        queues.push((key, Arc::clone(&queue)));
        Ok(queue)
    }

    async fn push(&self, key: String, message: Bytes) -> Result<()> {
        if message.len() > MESSAGE_MAX_LEN {
            return Err(Error::runtime(format!(
                "message exceeds {MESSAGE_MAX_LEN} bytes"
            )));
        }
        let queue = self.queue(key)?;
        self.run(async move { queue.send(message).await }).await
    }

    async fn receive(&self, key: String) -> Result<Bytes> {
        let queue = self.queue(key)?;
        self.run(async move { queue.receive().await }).await
    }

    async fn run<T>(&self, operation: impl core::future::Future<Output = T>) -> Result<T> {
        let id = self.next_operation.fetch_add(1, Ordering::Relaxed);
        let cancellation = Arc::new(CancellationFlag::new());
        {
            let mut lifecycle = self.lifecycle.lock();
            if !self.active.load(Ordering::Acquire) {
                return Err(revoked_error());
            }
            lifecycle.operations.push((id, Arc::clone(&cancellation)));
        }
        let _guard = OperationGuard { state: self, id };
        // Revocation wins over an operation that is ready in the same poll.
        let revoked = core::pin::pin!(Cancel::new(&cancellation).cancelled());
        let operation = core::pin::pin!(operation);
        match futures_util::future::select(revoked, operation).await {
            futures_util::future::Either::Left(_) => Err(revoked_error()),
            futures_util::future::Either::Right((output, _)) => Ok(output),
        }
    }

    fn revoke(&self) {
        let mut lifecycle = self.lifecycle.lock();
        self.active.store(false, Ordering::Release);
        for (_, cancellation) in lifecycle.operations.drain(..) {
            cancellation.cancel();
        }
    }
}

struct OperationGuard<'a> {
    state: &'a SharedState,
    id: usize,
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.state
            .lifecycle
            .lock()
            .operations
            .retain(|(id, _)| *id != self.id);
    }
}

impl Package for MessageQueuePackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let push = Arc::clone(&self.state);
        let receive = Arc::clone(&self.state);
        lua.register_lib("message_queue", move |package| {
            package.register_async("push", move |(key, message): (String, Bytes)| {
                let state = Arc::clone(&push);
                async move { Some(state.push(key, message).await) }
            })?;
            package.register_async("receive", move |key: String| {
                let state = Arc::clone(&receive);
                async move { Some(state.receive(key).await) }
            })
        })
    }
}

impl LuaPackage for MessageQueuePackage {
    fn name(&self) -> &'static str {
        "message_queue"
    }

    fn revoke(&self) {
        self.state.revoke();
    }
}

fn revoked_error() -> Error {
    Error::runtime("message queue package has been revoked")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use alloc::vec;
    use core::pin::pin;
    use futures_lite::future::{block_on, poll_once, zip};

    use super::*;

    fn bytes(value: &[u8]) -> Bytes {
        Bytes::copy_from(value).expect("allocate message")
    }

    fn lua_with(package: &MessageQueuePackage) -> Lua {
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");
        lua
    }

    #[test]
    fn declaration_depends_on_vm() {
        use barracuda_plugin::manager::PluginDeclaration;

        assert_eq!(MessageQueuePlugin::ID, "message-queue");
        assert_eq!(MessageQueuePlugin::DEPENDS_ON, &["vm"]);
    }

    #[test]
    fn package_is_require_only_and_binary_safe() {
        let package = MessageQueuePackage::new();
        let mut lua = lua_with(&package);
        let bytes: Vec<u8> = block_on(lua.load(
            "assert(message_queue == nil)\nlocal q = require('message_queue')\nq.push('key', '\\0\\255\\1')\nreturn q.receive('key')"
        ).eval_async()).expect("exchange bytes");
        assert_eq!(bytes, vec![0, 255, 1]);
    }

    #[test]
    fn fifo_and_key_isolation_are_shared_between_lua_states() {
        let package = MessageQueuePackage::new();
        let mut producer = lua_with(&package);
        let mut consumer = lua_with(&package);
        block_on(producer.load(
            "local q=require('message_queue'); q.push('a','one'); q.push('b','other'); q.push('a','two')"
        ).exec_async()).expect("produce");
        let values: (Vec<u8>, Vec<u8>, Vec<u8>) = block_on(consumer.load(
            "local q=require('message_queue'); return q.receive('a'), q.receive('a'), q.receive('b')"
        ).eval_async()).expect("consume");
        assert_eq!(
            values,
            (b"one".to_vec(), b"two".to_vec(), b"other".to_vec())
        );
    }

    #[test]
    fn empty_receive_waits_until_push() {
        let state = Arc::new(SharedState::new());
        block_on(async {
            let receive = state.receive("key".into());
            let push = async {
                state
                    .push("key".into(), bytes(b"ready"))
                    .await
                    .expect("push");
            };
            let (message, ()) = zip(receive, push).await;
            assert_eq!(&*message.expect("receive"), b"ready");
        });
    }

    #[test]
    fn full_queue_applies_backpressure_until_receive() {
        let state = Arc::new(SharedState::new());
        block_on(async {
            for value in 0..QUEUE_DEPTH {
                state
                    .push("key".into(), bytes(&[value as u8]))
                    .await
                    .expect("fill");
            }
            let mut blocked = pin!(state.push("key".into(), bytes(b"last")));
            assert!(poll_once(blocked.as_mut()).await.is_none());
            assert_eq!(&*state.receive("key".into()).await.expect("receive"), [0]);
            blocked.await.expect("unblocked push");
        });
    }

    #[test]
    fn limits_return_clear_errors() {
        let state = Arc::new(SharedState::new());
        block_on(async {
            assert!(state.push(String::new(), bytes(&[])).await.is_err());
            assert!(
                state
                    .push("k".repeat(KEY_MAX_LEN + 1), bytes(&[]))
                    .await
                    .is_err()
            );
            assert!(
                state
                    .push("large".into(), bytes(&[0; MESSAGE_MAX_LEN + 1]))
                    .await
                    .is_err()
            );
            for index in 0..MAX_KEYS {
                state
                    .push(format!("key-{index}"), bytes(&[]))
                    .await
                    .expect("create key");
            }
            assert!(state.push("overflow".into(), bytes(&[])).await.is_err());
        });
    }

    #[test]
    fn revocation_rejects_old_callbacks_and_wakes_waiters() {
        let package = MessageQueuePackage::new();
        let mut lua = lua_with(&package);
        package.revoke();
        let rejected: bool = block_on(lua.load(
            "local q=require('message_queue'); local v,e=q.push('k','v'); return v==nil and type(e)=='string'"
        ).eval_async()).expect("run callback");
        assert!(rejected);

        let state = Arc::new(SharedState::new());
        block_on(async {
            for _ in 0..QUEUE_DEPTH {
                state.push("full".into(), bytes(&[])).await.expect("fill");
            }
            let mut push = pin!(state.push("full".into(), bytes(&[])));
            let mut receive = pin!(state.receive("empty".into()));
            assert!(poll_once(push.as_mut()).await.is_none());
            assert!(poll_once(receive.as_mut()).await.is_none());
            state.revoke();
            assert!(push.await.is_err());
            assert!(receive.await.is_err());
        });
    }

    #[test]
    fn concurrent_producers_and_consumers_preserve_every_message_once() {
        let state = Arc::new(SharedState::new());
        block_on(async {
            let producer = |range: core::ops::Range<u8>| {
                let state = Arc::clone(&state);
                async move {
                    for value in range {
                        state
                            .push("shared".into(), bytes(&[value]))
                            .await
                            .expect("push");
                    }
                }
            };
            let consumer = || {
                let state = Arc::clone(&state);
                async move {
                    let mut values = Vec::new();
                    for _ in 0..16 {
                        values.push(state.receive("shared".into()).await.expect("receive")[0]);
                    }
                    values
                }
            };
            let (((), ()), (mut first, second)) = zip(
                async { zip(producer(0..16), producer(16..32)).await },
                zip(consumer(), consumer()),
            )
            .await;
            first.extend(second);
            first.sort_unstable();
            assert_eq!(first, (0..32_u8).collect::<Vec<_>>());
        });
    }
}
