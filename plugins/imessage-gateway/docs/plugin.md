# IMessage Gateway Plugin

- Plugin ID: `imessage-gateway`
- Direct dependency: `workflow`
- Provided capability: `IMessageGateway`
- Required capabilities: `WorkflowService`, `WorkflowActionRegistry`
- Workflow Actions: `gateway.send`, `gateway.send_stream`, `gateway.send_media`
- Workflow Events: `gateway.message.received`,
  `gateway.send_stream.finished`, `gateway.send_media.finished`
- Agent Tools: none
- Owned tasks: four text stream workers and four media workers
- Storage: none

`IMessageGateway` is the shared typed API. Provider Plugins register channels
and publish inbound messages through it; Workflow Actions and the separate
`agent-imessage-gateway` adapter call the same send methods.

## Ingress backpressure

`IMessageGateway::ready(&self)` (async) waits until Workflow can take another
`gateway.message.received`. It is `WorkflowService::ready_for` for that Event:
it resolves while fewer than `EVENT_BACKLOG_LIMIT` (4) executions started by the
Event are queued or running, and at once when no Workflow listens. It is
cancellation-safe. Receive loops await it before fetching each batch, so a burst
waits upstream instead of in RAM.

## Channel plumbing crates

Two library crates hold what every external channel Plugin (Telegram, WeChat,
QQ, BlueBubbles, Inkbox) shares. They are not part of this Plugin's
capability: the base Gateway does not depend on the webserver or the captive
portal, and only channel Plugins, which already do, use them.

### `barracuda-imessage-gateway-owners` (`crates/owners`)

Owner book per channel: at most `MAX_OWNERS` (8) senders may command the
device; everyone else is dropped and counted.

```rust
pub struct PairingEntropy;                     // type-erased Platform Entropy
impl PairingEntropy {
    pub fn new<E: barracuda_platform::Entropy>(entropy: E) -> Self;
    pub fn unavailable() -> Self;
}
pub struct Owner;                              // {"id":"…","label":"…"|null}
impl Owner {
    pub fn new(id: &str, label: Option<&str>) -> Option<Self>; // id 1..=128 bytes
    pub fn id(&self) -> &str;
    pub fn label(&self) -> Option<&str>;      // trimmed, ≤ 64 bytes
}
pub enum Classification { Owner, Paired, Ignored }
pub struct Owners<Storage>;                    // persisted under key "owners"
impl<Storage: PluginStorage> Owners<Storage> {
    pub async fn load(storage: Storage, entropy: PairingEntropy) -> Result<Self, OwnersError>;
    pub async fn classify(&self, sender: &str, label: Option<&str>, text: &str) -> Classification;
    pub fn is_owner(&self, id: &str) -> bool;
    pub fn count(&self) -> usize;
    pub fn ignored(&self) -> u32;
    pub fn owners(&self) -> Vec<Owner>;
    pub fn pairing(&self) -> Option<PairingView>;   // mints when none is valid
    pub fn rotate(&self) -> Result<(), EntropyUnavailable>;
    pub async fn remove(&self, id: &str) -> Result<bool, OwnersError>;
}
pub const PAIRED_REPLY: &str; // "已绑定，可以开始对话了\nPaired. You can start chatting."
```

- A pairing code is six digits drawn uniformly from the entropy source, valid
  for `PAIRING_CODE_LIFETIME` (10 minutes) and compared in constant time.
- A message pairs when its trimmed text is the code, or `/start <code>` (also
  `/start@bot <code>`).
- Pairing adds the sender as an owner and retires the code; the next
  `pairing()` mints a new one. `rotate()` mints a new one on request.
- `MAX_PAIRING_ATTEMPTS` (5) wrong code-shaped guesses also retire the code.
- No code is offered while the list is full or the Platform has no entropy.
- The list is stored as a JSON array; the code and the ignored counter are RAM
  only.
- `OwnerBook` is the same logic without storage, taking the time explicitly.

### `barracuda-imessage-gateway-channel` (`crates/channel`)

It re-exports the owners crate (as `owners`, plus its main types).

**Mode.**

```rust
#[serde(rename_all = "snake_case")]
pub enum ChannelMode { Disabled, Send, #[default] SendReceive }
impl ChannelMode {
    pub const ALL: &[Self];                  // disabled, send, send_receive
    pub const WITHOUT_SEND_ONLY: &[Self];    // WeChat: disabled, send_receive
    pub const fn legacy() -> Self;           // Send: configs stored before modes
    pub const fn registers(self) -> bool;    // not Disabled
    pub const fn receives(self) -> bool;     // SendReceive
}
pub async fn load_mode<S: PluginStorage>(storage: &S, configured: bool, legacy: ChannelMode)
    -> Result<ChannelMode, StorageError>;
pub async fn store_mode<S: PluginStorage>(storage: &S, mode: ChannelMode)
    -> Result<(), StorageError>;
```

The mode is stored under its own key, `mode`, as a JSON string. When no mode is
stored, `load_mode` returns `legacy` for an already configured channel and
`send_receive` for an unconfigured one, and stores the result. WeChat passes
`ChannelMode::SendReceive` as `legacy`.

**Receive-loop scaffold.**

```rust
pub trait ReceiveSlotSource: 'static {
    type Lease: 'static;                     // dropping it frees the slot
    fn acquire(&self) -> Option<Self::Lease>;
    fn capacity(&self) -> usize;
}
pub struct UnlimitedSlots;                   // Lease = (); webhook receivers
pub trait ReceiveChannel<Lease>: 'static {
    fn receive<'a>(&'a self, lease: &'a mut Lease, session: ReceiveSession<'a>)
        -> ReceiveFuture<'a>;                // Result<(), ReceiveError>
}
impl ReceiveSession<'_> { pub fn receiving(&self); }
impl ReceiveError {
    pub fn new(message: impl Into<String>) -> Self;   // retried after backoff
    pub fn halt(message: impl Into<String>) -> Self;  // waits for restart()
    pub fn retry_after(self, delay: Duration) -> Self; // ≥ delay, ≤ 10 min
}
pub enum ReceiveState { Idle, Starting, Receiving, NoSlot, Error(String) }
pub struct ReceiveControl<Slots: ReceiveSlotSource>;
impl<Slots: ReceiveSlotSource> ReceiveControl<Slots> {
    pub fn new(slots: Slots) -> Self;
    pub fn set_enabled(&self, enabled: bool) -> Result<(), NoSlot>;
    pub fn restart(&self);
    pub fn is_enabled(&self) -> bool;
    pub fn state(&self) -> ReceiveState;
    pub fn capacity(&self) -> usize;
}
pub struct ReceiveTiming { initial_backoff, max_backoff, slot_retry }
// ReceiveTiming::DEVICE = 1 s, 30 s, 5 s
pub fn receive_runtime<Slots, Channel>(control: Rc<ReceiveControl<Slots>>,
    channel: Rc<Channel>, timing: ReceiveTiming) -> ReceiveRuntime;
```

- The runtime parks on a signal while receiving is disabled and holds no slot.
- `set_enabled(true)` takes a slot at once, so the caller can answer
  `no_slot`. Without a free slot the control stays enabled, reports `no_slot`,
  and the runtime retries every `slot_retry`.
- While enabled, the runtime runs the channel's session over the lease. It
  reconnects at once after a session that called `receiving()`.
- After an error it waits 1 s, doubling to a 30 s cap; the wait resets once a
  session reports `receiving()`.
- `halt` errors wait for `restart()` or a mode change.
- `set_enabled(false)` drops the session and the lease.
- Dropping the runtime, as task cancellation does, drops the session and the
  lease, reports `idle`, and stops the control from taking slots.

The System's receive slot pool reaches a channel through a small
`ReceiveSlotSource` adapter in the channel Plugin.

**HTTP surface.** The channel implements:

```rust
pub trait ChannelControl: 'static {
    type Storage: PluginStorage;
    type Slots: ReceiveSlotSource;
    fn configured(&self) -> bool;
    fn mode(&self) -> ChannelMode;
    fn modes(&self) -> &'static [ChannelMode] { ChannelMode::ALL }
    fn apply_mode(&self, mode: ChannelMode) -> ModeFuture<'_>; // Result<(), ModeError>
    fn receive(&self) -> &ReceiveControl<Self::Slots>;
    fn owners(&self) -> &Owners<Self::Storage>;
}
pub fn sync_receive<C: ChannelControl + ?Sized>(channel: &C) -> Result<(), NoSlot>;
pub fn status_response<C: ChannelControl + ?Sized>(channel: &C) -> HttpResponse;
pub struct ModeEndpoint<C>;   // ModeEndpoint::new(Rc<C>): HttpEndpoint
pub struct OwnersEndpoint<C>; // OwnersEndpoint::new(Rc<C>): HttpEndpoint
```

| Request | Response |
| --- | --- |
| `GET /api/gateway/<channel>` | 200 `{"configured":true,"mode":"send_receive","receive":{"state":"receiving"},"owners":{"count":1}}` |
| `POST /api/gateway/<channel>/mode` `{"mode":"send"}` | 204 |
| the same with `send_receive` and no free slot | 409 `{"error":"no_slot","capacity":2}` |
| `GET /api/gateway/<channel>/owners` | 200 `{"owners":[{"id":"42","label":"Ann"}],"pairing":{"code":"012345","expires_in":600},"ignored":3}` |
| `POST /api/gateway/<channel>/owners` `{"remove":"42"}` or `{"rotate":true}` | 204 |

- `receive` is present only in `send_receive`. `state` is one of `idle`,
  `starting`, `receiving`, `no_slot`, or `error`. `message` comes only with
  `error`, and `capacity` only with `no_slot`.
- A 409 still saves the mode; receive then reports `no_slot` until a slot
  frees.
- `pairing` is `null` while the owner list is full or the Platform has no
  entropy. `GET` mints a code when none is valid.
- Removing an unknown id is a 204 as well.
- Errors:
  - 400 `{"error":"invalid_request"}` for a malformed body;
  - 400 `{"error":"unsupported_mode"}` for a mode the channel does not offer;
  - 422 `{"error":"registration_failed"}`;
  - 500 `{"error":"storage"}`;
  - 503 `{"error":"entropy_unavailable"}` when a rotation cannot mint a code;
  - 405 `{"error":"method_not_allowed"}` for other methods.

**Portal status.** `entry_status(&channel)` (or
`channel_entry_status(configured, mode, &state)`) gives the
`barracuda_captive_portal_plugin::EntryStatus`:

| Channel | Status |
| --- | --- |
| not configured | `off` 未配置/Not set up |
| `disabled` | `off` 已停用/Disabled |
| `send` | `ready` 仅发送/Send only |
| `send_receive`, receiving | `ready` 收发中/Receiving |
| `send_receive`, idle or starting | `attention` 连接中/Connecting |
| `send_receive`, no slot | `attention` 名额已满/No slot |
| `send_receive`, error | `attention` 连接中断/Disconnected |

## How a channel plugs in

1. **Dependencies.** Depend on `barracuda-imessage-gateway-channel` (path
   `../../../imessage-gateway/crates/channel`) and on
   `barracuda-platform` for `Entropy`.
2. **Constructor.** Take the Platform entropy and the receive slots:
   `new(context, entropy: impl barracuda_platform::Entropy)`, keep
   `PairingEntropy::new(entropy)`, and wrap the receive pool from
   `PluginContext` in a `ReceiveSlotSource` adapter. BlueBubbles, which
   receives by webhook, uses `UnlimitedSlots`. The System must pass
   `prepared.entropy.clone()` to the constructor: add the Plugin to the
   entropy case in `tools/plugin-tool` and regenerate.
3. **State.** In `register`:
   - load the configuration;
   - `load_mode(storage, configured, ChannelMode::legacy())` (WeChat:
     `ChannelMode::SendReceive`);
   - `Owners::load(storage.clone(), entropy)`;
   - register the `MessageChannel` only when `configured && mode.registers()`;
   - build `Rc<ReceiveControl<_>>`;
   - put all of this in one channel-state type that implements
     `ChannelControl`. WeChat overrides `modes()` with
     `ChannelMode::WITHOUT_SEND_ONLY`.
4. **`apply_mode`.** Call `store_mode`, then add or drop the Gateway
   registration so that one exists exactly when the channel is configured and
   `mode.registers()`, then update `mode()`. Map failures to
   `ModeError::Storage` or `ModeError::Registration`.
5. **Receiver.** Implement `ReceiveChannel<Lease>`. Each session loops:
   1. await `IMessageGateway::ready()`;
   2. fetch one batch over the lease;
   3. call `session.receiving()`;
   4. for each new message (deduplicated), call
      `owners.classify(sender, label, text)`:
      - `Owner`: `gateway.publish(...)`;
      - `Paired`: send `PAIRED_REPLY` through the channel's normal send path;
      - `Ignored`: nothing.
   5. persist the cursor.

   Return `ReceiveError::new` for failures that a retry can fix, `halt` for
   ones it cannot (WeChat `-14`: 「需要重新扫码」), and `.retry_after(..)` for
   429s.
6. **Runtime.** Build
   `receive_runtime(control, Rc::new(receiver), ReceiveTiming::DEVICE)`, then
   call `sync_receive(&*state)`; a `NoSlot` there is already reported as
   state. Keep the runtime for `start`.
7. **Endpoints.**
   - The configuration endpoint's `GET` returns `status_response(&*state)`.
   - After a successful configuration `POST`, it calls `sync_receive` and
     `control.restart()`.
   - Register `/api/gateway/<channel>/mode` with `ModeEndpoint::new(state)`
     and `/api/gateway/<channel>/owners` with `OwnersEndpoint::new(state)`.
8. **Portal.** `register_with_status(.., move || entry_status(&*state))`.
9. **Task.** In `start`, spawn one task that owns the runtime:

   ```rust
   #[embassy_executor::task]
   async fn telegram_receive_task(runtime: ReceiveRuntime, cancellation: PluginTaskToken) {
       let _completed = select(cancellation.cancelled(), runtime).await;
   }
   ```
10. **Docs.** In the channel's `plugin.md`, declare the task, the endpoints, the
    storage keys (`mode`, `owners`, and the cursor), and the slot use.
