# Router examples

`component_rpc` shows the complete Component contract:

1. `CounterService::register` installs a typed RPC through `RegisterContext`.
2. `StartupCaller::run` obtains the shared client from `RunContext` and calls it.
3. The application explicitly polls `Router` until the observable result is ready.
4. Both Components are synchronously unloaded and their teardown is verified.

```console
cargo run -p barracuda-router --example component_rpc
```
