# Workflow examples

`definition_matching` constructs three immutable Workflow definitions, matches
an Event against exact and wildcard rules in definition load order, inspects
their ordered RPC steps, and demonstrates empty-step validation.

```console
cargo run -p barracuda-workflow --example definition_matching
```
