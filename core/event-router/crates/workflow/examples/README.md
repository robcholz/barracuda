# Workflow examples

`definition_matching` constructs three immutable Workflow definitions, matches
an Event against exact and wildcard rules in definition load order, inspects
their ordered RPC steps, and demonstrates empty-step validation.

`event_input_mapping` constructs a one-step Workflow that calls `agent.run`
directly and maps fields from the triggering JSON Event with
`$event.input.<field>`.

```console
cargo run -p barracuda-workflow --example definition_matching
cargo run -p barracuda-workflow --example event_input_mapping
```
