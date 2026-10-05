# Barracuda Plugin Manager

`barracuda-plugin-manager` is the system boundary for Plugin identity,
capabilities, owned tasks, and persistent storage. Runtime composition stays in
`barracuda-system`; this core crate does not construct or own the application
System.

`barracuda-system` adds the complete Plugin set during startup. Plugin Manager
scans the declared dependency DAG, registers every Plugin in dependency order,
then starts the registered set in that same order. It does not expose
`load_plugin` or `unload_plugin` as a
runtime API. The manager's lifecycle operations are internal assembly
machinery, including rollback when one phase fails.

```text
add(imessage-web, webserver, imessage-gateway, ...)
                         |
                         v
register(webserver) -> register(imessage-gateway) -> register(imessage-web) -> ...
                                                                       |
                                                                       v
start(webserver)    -> start(imessage-gateway)    -> start(imessage-web)    -> ...
```

Both phases are synchronous. `register` atomically constructs the complete
typed capability graph without yielding. `start` is only an optional
post-registration hook: `PluginStartContext` cannot publish capabilities, and
task spawning itself is synchronous. Long-running services run as
owner-managed Embassy tasks under the repository's
[`execution-ownership.md`](../../../../.agents/docs/execution-ownership.md) boundary.
System installs the Embassy spawner directly on Plugin Manager; only
`PluginStartContext::task_spawner` exposes it, so registration cannot start a
service before the complete graph exists.

Async storage access and other I/O run in the Embassy task that owns that work.
Lifecycle methods may clone storage handles into those owners, but do not block
or await I/O themselves. `PluginManager::open` remains async because mounting
the shared database performs real storage I/O.

The manager validates `PluginDeclaration::ID`, derives one namespace-restricted storage
implementation from that stable identity, and passes it through
`PluginRegisterContext` as the `PluginStorage` contract. A Plugin may clone its
storage capability into its owned services and tasks. Keys are UTF-8 strings
and values are Plugin-owned fixed-layout zerocopy types; Plugin Manager adds no
serialization format of its own.

Plugins can also exchange runtime-only typed capabilities. A provider calls
`PluginRegisterContext::provide(Rc<T>)` during registration; a consumer
declares the provider in `PluginDeclaration::DEPENDS_ON` and calls
`PluginRegisterContext::require::<T>(provider)`. The manager stores the value as
`Rc<dyn Any>` under `(provider PluginId, TypeId)`. It has no knowledge of
concrete capability types, and these entries are never persisted to `ekv`.

System assembles statically selected Platform handles and derived shared
services into one `barracuda_plugin_api::PluginContext`. Every concrete Plugin
constructor receives a shared reference to that construction context and takes
or clones only the fixed handles it owns. `PluginRegisterContext` separately
carries Plugin-owned storage, declared Plugin capabilities, and retained
resources. The Embassy spawner remains an explicit startup lifecycle facility
rather than a construction resource or typed lookup.

Capabilities follow their provider's lifecycle. A provider cannot unload while
declared dependents remain registered, and phase rollback removes everything
published by that attempt. `PluginRegisterContext::retain` keeps registration
guards or other resources alive until the owning Plugin unloads successfully.

Each Plugin declares its own identity and constructs its capabilities and
owned tasks. `barracuda-system` knows which Plugins make up the application but
does not own their internal services.

```text
System
├── PluginContext { ip_stack, http_clients }
└── PluginManager
    ├── Typed capability registry
    │   └── (provider, TypeId) -> Rc<dyn Any>
    ├── Plugin-owned task cancellation
    └── ekv Database
        ├── PluginStorage("scheduler")
        └── PluginStorage("another-plugin")
```

Unloading cancels the Plugin's owned tasks, removes its capabilities, and drops
its retained resources. It does not delete the Plugin namespace; registering
the same `PluginId` again restores access to the same data.

System passes the writable database NOR capability projected by the selected
Platform from its Board-native layout to `PluginManager::open`. Plugin Manager constructs and
owns the single concrete `barracuda-kv::Database`; there is no interchangeable
storage-backend interface. Completely erased storage is formatted on first
boot, while non-erased storage is mounted without destructive fallback.

`PluginStorage` exposes only the scoped KV semantics. It contains no flash,
partition, filesystem, or Platform type. Public keys are `&str`, and values are
fixed-layout `barracuda-kv::Value` types backed by zerocopy. Its read and write
transaction contracts preserve ekv's lifecycle: writes are staged in key
order, an explicit `commit` makes them atomic, and dropping an uncommitted
transaction rolls them back. `get`, `put`, and `delete` remain one-operation
conveniences.
