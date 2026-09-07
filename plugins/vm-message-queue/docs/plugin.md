# Message Queue Plugin

- Plugin ID: `message-queue`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Owned Event Router Components: none
- Owned tasks: none

This VM Lua package Plugin registers the require-only `message_queue` package
for every Lua execution. Its package owns one shared queue map, so independent
Lua executions exchange binary-safe messages through the same key without any
VM-specific message-queue behavior.

Lua API:

- `message_queue.push(key, message)` asynchronously waits for queue capacity.
- `message_queue.receive(key) -> message` asynchronously waits for the next
  FIFO message.

The package supports multiple producers and consumers. Each key has an
independent FIFO with depth 8. The map accepts at most 16 keys, keys are limited
to 64 UTF-8 bytes, and messages are limited to 4096 bytes. These fixed limits
bound queued memory and prevent capacity growth; a full queue applies async
backpressure until a consumer frees a slot.

The Plugin exposes no typed capability, Event Router RPC (typed or dynamic), or
Plugin-owned task. Dropping its retained VM registration revokes callbacks in
existing Lua states. New calls then return a Lua runtime error, while revocation
actively cancels and wakes every push or receive waiting on a queue.
