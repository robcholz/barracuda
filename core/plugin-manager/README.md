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
register(webserver) -> register(message-gateway) -> ...
                                             |
                                             v
start(webserver)    -> start(message-gateway)    -> ...
```

Both phases are cooperative futures. `register` is preparation that must
finish before any Plugin starts and defaults to a no-op. `start` performs the
Plugin's actual initialization. A Plugin starts long-running work by loading a
Component; Event Router remains responsible for polling Component futures.

The manager validates `Plugin::id()`, derives one namespace-restricted
`ScopedStorage` from that stable identity, and passes it through
`PluginContext`. A Plugin may clone its storage capability into any number of
Components. Keys and values remain raw bytes; serialization and data layout are
entirely Plugin-owned.

Plugins can also exchange runtime-only typed capabilities. A provider calls
`PluginContext::provide(Rc<T>)`; a consumer declares the provider in
`Plugin::DEPENDS_ON` and calls `PluginContext::require::<T>(provider)`. The
manager stores the value as `Rc<dyn Any>` under `(provider PluginId, TypeId)`.
It has no knowledge of concrete capability types, and these entries are never
persisted to `ekv`.

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
    ├── Typed capability registry
    │   └── (provider, TypeId) -> Rc<dyn Any>
    └── ekv Database
        ├── ScopedStorage("scheduler")
        └── ScopedStorage("another-plugin")
```

The Event Router still owns Component registration, execution, and teardown.
The Plugin layer records the Component identities belonging to each Plugin so
they can be rolled back or unloaded as a group. Unloading does not delete the
Plugin namespace; registering the same `PluginId` again restores access to the
same data.

The application owns raw flash setup. It creates an `EkvStore`, mounts an
existing database or explicitly formats new storage, then moves the store into
`PluginManager`. Formatting policy is deliberately outside Plugin code.

`ScopedStorage` exposes raw `get`, `put`, `delete`, and atomic multi-key
`commit` operations. It automatically prefixes every key, validates the
remaining `ekv` key capacity, and sorts atomic batches as required by `ekv`.
