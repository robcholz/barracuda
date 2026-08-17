# Event Router heap profiles

Run one deterministic scenario per process from the repository root:

```bash
cargo run --profile profiling -p barracuda-event-router-profile -- rpc-unary
cargo run --profile profiling -p barracuda-event-router-profile -- router-lifecycle
cargo run --profile profiling -p barracuda-event-router-profile -- workflow-catalog
```

Each command prints allocation volume, peak live memory, memory retained while
the workload is alive, and memory retained after its runtime owners are dropped.
DHAT reports are written under `target/profiles/event-router/` by default. Pass a
second argument to select another output file.

The scenarios intentionally use statically allocated lane storage. The workflow
scenario clears its in-memory persistence fixture before the `after_drop`
snapshot, so fixture bytes do not look like runtime retention.

DHAT measures requested allocations and allocation lifetimes. It cannot measure
allocator free-list geometry or external fragmentation. Device fragmentation
still requires platform snapshots containing total free bytes and largest free
block bytes.
