# RPC examples

| Example | Purpose |
| --- | --- |
| `rpc_typed` | Typed unary/streaming calls and zero-copy frames |
| `rpc_payload` | Runtime-addressed payload writer/reader calls |
| `rpc_multicast` | Shared requests with independent response branches |
| `rpc_nested` | Nested calls, call identity, and self-call protection |
| `rpc_json` | Runtime `call_json` over `#[rpc_dynamic]` methods |

Run from the `agent-framework` workspace root:

```console
cargo run -p barracuda-rpc --example rpc_typed
cargo run -p barracuda-rpc --example rpc_payload
cargo run -p barracuda-rpc --example rpc_multicast
cargo run -p barracuda-rpc --example rpc_nested
cargo run -p barracuda-rpc --example rpc_json
```
