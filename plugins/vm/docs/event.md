# Lua VM Events

The VM Component emits no Events.

Lua execution is a call-scoped bidirectional data flow, so source, input,
captured output, completion, failure, and cancellation all belong to `vm.run`.
Publishing any of them as Events would discard the call's ownership and
backpressure semantics.
