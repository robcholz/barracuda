# Claw Rust Framework

## Layout

- `crates/` contains production Rust crates.
- `bench/` contains host-only measurement tooling and profiling workloads.

The memory profiler is an executable workload rather than a throughput
benchmark:

```bash
cargo run --profile profiling -p claw-agent-profile -- agent-init
```

## Python tooling

Python tools are members of the root uv workspace and share one `uv.lock` and
one `.venv`. Set up every tool and its development dependencies from this
directory:

```bash
uv sync --all-packages --all-groups
```

Select packaged tools explicitly with `--package`, so member selection never
depends on the current directory. The examples below run from this workspace
root so their input/output paths are also unambiguous. Standalone PEP 723
scripts continue to use `uv run --script`.

## Debugging

### Export Trace

```bash
uv run --package claw-trace \
  claw-trace-chrome <path-to-log> -o <where-you-want-to-emit-chrome-trace>
```

Visualization

[Perfetto UI](https://ui.perfetto.dev/)

The command uses `claw-log`'s canonical Python exporter. Its synthetic Chrome
process/thread mapping (including `run.system`, session grouping, and the
`unattributed` fallback) is documented in
[`crates/claw-log/scripts/README.md`](crates/claw-log/scripts/README.md).

### Context Visualization

```bash
uv run --script crates/claw-context/scripts/context_viewer.py
```

## Prebuilt ESP-IDF static library

Build the complete Rust runtime as a target-specific static archive:

```bash
./build-idf-staticlib.sh
```

Each run builds archives for `esp32s3`, `esp32c5`, `esp32p4`, and `esp32s31`.
The archives are written to
`crates/claw-cabi/prebuilt/<idf-target>/libclaw_cabi.a`. These archives are
committed with the repository, and ESP-IDF links the archive matching
`IDF_TARGET` by default. No Rust toolchain is needed unless the Rust sources
change.

CI verifies that all committed archives match the Rust sources without
overwriting them:

```bash
./build-idf-staticlib.sh --check
```

Build the firmware normally:

```bash
idf.py -C ../application/edge_agent build
```

Set `CLAW_DEBUG=1` while running the archive build script to enable rich
logging. To bypass the committed archives during Rust development, configure
with an empty prebuilt directory:

```bash
idf.py \
  -C ../application/edge_agent \
  -DCLAW_RUST_PREBUILT_DIR= \
  build
```

## Flashing

Enable rich logging by:

```bash
export CLAW_DEBUG=1
```

Clear it with:

```bash
unset CLAW_DEBUG
```
