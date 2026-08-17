# Event Router throughput benchmark

This directory is the canonical throughput benchmark for
`EventRouter<N, M, Q>`. It is an internal `[[bench]]` target on the Event Router
crate, with `harness = false`; it is neither an example nor a standalone
workspace package. The uv-driven pipeline builds it with Cargo's optimized
`bench` profile, records raw CSV, validates sample completeness and checksums,
and writes both machine-readable and Markdown summaries.

This benchmark does not measure allocations, heap peaks, or retained memory.
Those measurements belong to the separate `../profile/` benchmark family.

Throughput is reported in bytes per second:

- `ingress_bytes_per_second = events * payload_bytes / elapsed_seconds`
- `routed_bytes_per_second = ingress_bytes * matched_fanout / elapsed_seconds`

The timed interval starts after warmup and ends only after every matched sink
has completed. Build time and workflow setup are excluded.

## Run it

From the workspace root:

```console
# Fast representative smoke run (six configurations, seven samples each)
uv run python core/event-router/bench/throughput/run.py --suite quick

# Full N/M/Q, payload, fan-out, and slow-consumer experiment
uv run python core/event-router/bench/throughput/run.py --suite full

# One-sample pipeline validation
uv run python core/event-router/bench/throughput/run.py --suite quick --samples 1
```

The runner prints the output directory. By default it creates a timestamped
directory below `.gstack/benchmark-reports/`. Each run contains:

- `raw-*.csv`: every measured sample
- `metadata.json`: commit, dirty-source fingerprint, host, compiler, and command
- `summary.json`: medians, inclusive IQR, CV, and normalized scores
- `report.md`: a readable table

Use `--output PATH` to select a new empty output directory. The runner refuses
to overwrite existing benchmark artifacts and aborts if Event Router source
changes during the build or measurement.

## Detect a regression

Use an earlier `summary.json` as the baseline. The command exits with status 2
when any exactly matching configuration falls below the accepted ratio:

```console
uv run python core/event-router/bench/throughput/run.py \
  --suite quick \
  --baseline .gstack/benchmark-reports/event-router-throughput-BASELINE/summary.json \
  --minimum-ratio 0.90
```

Only compare runs made on equivalent hardware and under similar system load.
Use the default seven samples for decisions; `--samples 1` is only a smoke test.

## Parameter interpretation

- `N` is the number of concurrent RPC lanes. For matched routing it must exceed
  fan-out. Start near `fanout + 1`; increase it for concurrent producers or
  slow consumers, then confirm with the `joint` and `slow_n` scenarios.
- `M` is the byte capacity of each request and response pipe. It reserves about
  `N * 2 * M` payload bytes, so the smallest non-fragmenting value is normally
  best. On the current 64-bit layout, one Event payload frame has 26 bytes of
  ingress metadata; test around `payload + 26` with the `fine-m` suite.
- `Q` only bounds root calls waiting for a lane. Keep `Q = 0` when backpressure
  is acceptable; raise it when callers must queue bursts. It is not a general
  throughput knob.

Treat these as starting rules, not constants: run the full suite for the actual
payload distribution, fan-out, and sink latency of a deployment.

## Maintain the pipeline

The Rust benchmark workload lives in `main.rs`; suite orchestration is in
`run.py`, and CSV validation/reporting is in `analyze.py`. The target is declared
in `core/event-router/Cargo.toml` beside the internal `profile` bench target.

The uv runner is the canonical entrypoint because it preserves raw data and
metadata. For workload development, the underlying Cargo benchmark target can
also be invoked directly:

```console
cargo bench -p barracuda-event-router --bench throughput -- \
  --suite quick --samples 1
```

Run the pipeline unit tests with:

```console
uv run python -m unittest discover -s core/event-router/bench/throughput -p 'test_*.py'
```

When adding a scenario, give every configuration a stable scenario name and
include enough events that scheduler noise is small relative to elapsed time.
Do not remove warmup, matched-completion waiting, checksum validation, or the
source fingerprint checks.
