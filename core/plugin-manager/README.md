# Barracuda Plugin Manager

`barracuda-plugin-manager` is the system boundary for grouping Components and
assigning persistent storage ownership. Runtime composition stays in
`barracuda-system`; this core crate does not construct or own the application
System.

`barracuda-system` selects the complete Plugin set during startup. It first
registers every Plugin in dependency order, then starts the registered set in
that same order. It does not expose `load_plugin` or `unload_plugin` as a
runtime API. The manager's lifecycle operations are internal assembly
machinery, including rollback when one phase fails.

```text
register(webserver) -> register(imessage-gateway) -> register(imessage-web) -> ...
                                                                       |
                                                                       v
start(webserver)    -> start(imessage-gateway)    -> start(imessage-web)    -> ...
```

Both phases are cooperative futures. `register` is preparation that must
finish before any Plugin starts and defaults to a no-op. `start` performs the
Plugin's actual initialization. A Plugin starts long-running work by loading a
Component; Event Router remains responsible for polling Component futures.

The manager validates `Plugin::id()`, derives one namespace-restricted storage
implementation from that stable identity, and passes it through
`PluginContext` as the `PluginStorage` contract. A Plugin may clone its storage
capability into any number of Components. Keys are UTF-8 strings and values are
Plugin-owned fixed-layout zerocopy types; Plugin Manager adds no serialization
format of its own.

Plugins can also exchange runtime-only typed capabilities. A provider calls
`PluginContext::provide(Rc<T>)`; a consumer declares the provider in
`Plugin::DEPENDS_ON` and calls `PluginContext::require::<T>(provider)`. The
manager stores the value as `Rc<dyn Any>` under `(provider PluginId, TypeId)`.
It has no knowledge of concrete capability types, and these entries are never
persisted to `ekv`.

System installs statically selected Platform handles separately with
`PluginManager::provide_system`. A Plugin obtains them during registration or
startup through `PluginContext::require_system::<T>()`. This lookup exists only
at composition time: the Plugin clones the concrete handle into its Component,
so filesystem and network calls remain statically dispatched.

Capabilities follow their provider's lifecycle. A provider cannot unload while
declared dependents remain registered, and phase rollback removes everything
published by that attempt. `PluginContext::retain` keeps registration guards or
other resources alive until the owning Plugin unloads successfully.

Each Plugin declares its own identity, constructs its own Components, and calls
`PluginContext::load`. `barracuda-system` therefore knows which Plugins make up
the application but does not know their IDs or which Components they contain.

```text
System
└── PluginManager
    ├── EventRouter registrar
    │   └── Plugin -> Component, Component, ...
    ├── Typed capability registries
    │   ├── System TypeId -> Rc<dyn Any>
    │   └── (provider, TypeId) -> Rc<dyn Any>
    └── ekv Database
        ├── PluginStorage("scheduler")
        └── PluginStorage("another-plugin")
```

The Event Router still owns Component registration, execution, and teardown.
The Plugin layer records the Component identities belonging to each Plugin so
they can be rolled back or unloaded as a group. Unloading does not delete the
Plugin namespace; registering the same `PluginId` again restores access to the
same data.

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
