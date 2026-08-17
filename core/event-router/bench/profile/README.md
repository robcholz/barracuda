# Event Router heap profiles

Run one deterministic scenario per process from the repository root:

```bash
cargo bench --profile profiling -p barracuda-event-router --bench profile -- rpc-unary
cargo bench --profile profiling -p barracuda-event-router --bench profile -- router-lifecycle
cargo bench --profile profiling -p barracuda-event-router --bench profile -- workflow-catalog
```

Each command prints allocation volume, peak live memory, memory retained while
the workload is alive, and memory retained after its runtime owners are dropped.
DHAT reports are written under `target/profiles/event-router/` by default. Pass a
second argument to select another output file.

This profiler does not report events/s or bytes/s. Event Router throughput and
its regression baselines live separately under `../throughput/`.

The scenarios intentionally use statically allocated lane storage. The workflow
scenario clears its in-memory persistence fixture before the `after_drop`
snapshot, so fixture bytes do not look like runtime retention.

DHAT measures requested allocations and allocation lifetimes. It cannot measure
allocator free-list geometry or external fragmentation. Device fragmentation
still requires platform snapshots containing total free bytes and largest free
block bytes.
