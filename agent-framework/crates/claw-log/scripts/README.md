# claw-trace tooling

Capture a live `TRACE` log from a device monitor and export it as a Chrome trace
you can open in `chrome://tracing` / Perfetto. The grammar these tools parse is
[`../docs/trace-format.md`](../docs/trace-format.md).

## CLI: `claw-trace-capture` (live capture)

Run a monitor as a child process, parse its `TRACE` lines live, and write
`trace.json` when it exits (or on Ctrl-C).

```bash
# from agent-framework/
uv run --package claw-trace \
  claw-trace-capture --cmd "cargo espflash monitor" -o trace.json --tee
```

| Option | Default | Meaning |
|--------|---------|---------|
| `--cmd` | — | Monitor command to run, e.g. `"cargo espflash monitor"` or `"idf.py monitor"`. |
| `-o` / `--output` | `trace.json` | Output Chrome Trace Event JSON path. |
| `--encoding` | `utf-8` | Encoding used to decode the monitor's bytes. |
| `--tee` | off | Also echo each captured line to stderr live (watch the log while capturing). |

Why a child process instead of a raw pipe: Ctrl-C is handled cleanly — the
monitor is terminated and the records collected so far are still exported.
Chrome export is a batch step (span-tree reconstruction needs the whole
capture), so the JSON is written once at the end. Occasional corrupt serial
lines are skipped (and counted) rather than aborting the capture.

Chrome's thread rows are synthetic lanes keyed by the trace protocol's logical
`task` label. For async runtime work they represent agent/future identity, not
the executor OS or FreeRTOS thread that happened to poll it.

Chrome's process rows are also synthetic grouping scopes:

- records with `run.session` use that session as the process name and require
  the inherited `run.system` scope;
- system-wide records with only `run.system` use the `agent-system` scope;
- records with neither key use `unattributed` (for example pre-init work or a
  boundary record emitted outside an entered runtime scope; it is not an
  “unknown session”);
- a session process key includes its system scope as well as its session id.

There is no legacy trace compatibility. A record with `run.session` but no
`run.system` violates the current prefix-closed context model and aborts export;
regenerate the capture with the current Rust runtime. This does not apply to a
record with neither key: that is a valid current record and remains
`unattributed`.

Chrome requires integer `pid`/`tid` values. The exporter assigns small integers
only as format-level ids; a viewer may append one (for example the `2` in
`session-5 (2)`) to a label. That number is not part of the session id and does
not mean a Rust thread.

Counter tracks are opt-in. Only an event field named
`counter.<series>=<number>` produces a Chrome `ph=C` record; the exported series
name has the `counter.` prefix removed. For example,
`counter.free_heap=120000` samples the `free_heap` series. Ordinary numeric
fields such as `argument_bytes=120`, `replace_count=2`, or `count=3` remain
instant-event arguments and never create counter tracks. An explicitly marked
counter whose value is not numeric aborts export.

## CLI: `claw-trace-chrome` (offline `.log` → `trace.json`)

Convert an already-captured `TRACE` log file (or stdin) to Chrome Trace Event
JSON you can open in `chrome://tracing` / Perfetto. Use this when you already
have a `.log` and don't need a live monitor.

```bash
# from agent-framework/
uv run --package claw-trace claw-trace-chrome device.log -o trace.json
cat device.log | uv run --package claw-trace \
  claw-trace-chrome --strip-ansi -o trace.json
```

| Option | Default | Meaning |
|--------|---------|---------|
| `file` | stdin | Log file to read; reads stdin when omitted. |
| `-o` / `--output` | `trace.json` | Output Chrome Trace Event JSON path. |
| `--strip-ansi` | off | Strip ANSI color escapes from each line before parsing. |

## Libraries

Three of the packages are importable libraries; `trace_capture` is the glue tool
above. The first three share `claw-log/pyproject.toml`; `serial_log` is a
standalone, dependency-free project with its own `pyproject.toml`.

| Package | Role |
|---------|------|
| `claw_trace` | Pure parser: `parse_line` / `parse` turn lines into `TraceRecord`s; `build_forest` rebuilds the span tree, timing, and inherited context; `render_tree` dumps it. No extra deps. |
| `chrome_export` | Consumes a `claw_trace` forest and writes Chrome Trace Event JSON (`write_chrome_trace`). Spans → duration slices, same-lane parents → slice nesting, cross-lane parents → native `span_parent` flows, events → instant events, explicit `counter.<series>` fields → counter tracks. Exposed as the `claw-trace-chrome` CLI. Depends on `chrometrace`. |
| `serial_log` | Standalone, dependency-free: `LogStream` runs a subprocess and pushes each complete stdout line to `on_log` callbacks (strips ANSI, configurable encoding). See `serial_log/README.md`. |
| `trace_capture` | Glue only: wires `serial_log` → `claw_trace` → `chrome_export` behind `claw-trace-capture`. |

`claw_trace` stays importable and is still runnable via `python -m claw_trace`
(pretty-prints the reconstructed span tree), but is intentionally not registered
as a console script.

`chrome_export` is the single canonical Chrome Trace translation path. Both
CLIs above call this Python package; there is no second Rust exporter whose
mapping could drift from it.

Every exported span slice carries its structural id in `args.span`. An instant
event inside a span carries the same enclosing id, while an orphan event omits
it. This preserves exact event-to-child joins in Perfetto without changing
slice timing or adding subsystem-specific exporter logic.

The exporter recognizes the generic `flow_link` event described in
[`../docs/trace-format.md`](../docs/trace-format.md). It emits a Chrome flow
from the enclosing span to a span selected by logical task and optional span
name. `flow.arg.*` fields are copied onto the source slice and both flow
endpoints for Perfetto search and SQL queries. Independently, every structural
span-parent edge crossing process/task lanes becomes a native Chrome flow named
`span_parent`; same-lane parents are rendered through nested complete slices.
Both kinds use one flow-id sequence. Since legacy Chrome flow starts cannot
name endpoint slice ids, each flow connects generated instant proxies on the
source and target lanes; their arguments identify both real span ids. Those
explicit ids remain authoritative if other slices temporally enclose the proxy
timestamps. If capture stops before a span exits, the exporter closes its
complete slice at the observable trace/ancestor boundary and marks it with
`incomplete=true`, avoiding unmatched duration events.

## Develop / test

```bash
# from agent-framework/
uv run --package claw-trace \
  pytest -c crates/claw-log/pyproject.toml crates/claw-log/scripts/tests
uv run --package claw-trace ruff format crates/claw-log
```
