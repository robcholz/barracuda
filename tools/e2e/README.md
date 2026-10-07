# barracuda-e2e

End-to-end harness for the host Barracuda System. Each scenario boots the real
`local-linux` System on a freshly erased flash image, drives it through the
`barracuda chat` CLI Channel and direct WebServer requests, serves every model
call from an llm-tape tape, and asserts on the reply transcript, tool results,
model-visible request bodies, and System logs.

```text
scenario.toml ──> tape (scripted or recorded) ──> llm-tape replay (0.0.0.0:18787)
                                                        ^
barracuda chat ──WebSocket──> System (10.42.0.2:8787) ──┘ model API at 10.42.0.1
      │                          │
 transcript.jsonl            system.log   ──> assertions
```

## Prerequisites

```bash
cargo board select local-linux
sudo sh platforms/linux/provision.sh barracuda0
uv sync --all-packages --all-groups
```

## Run

```bash
uv run --package barracuda-e2e barracuda-e2e list
uv run --package barracuda-e2e barracuda-e2e run                # all scenarios
uv run --package barracuda-e2e barracuda-e2e run tool-vm --skip-build
```

`run` builds the System, the CLI, and the Plugin resources image unless
`--skip-build` is given. Scenarios run sequentially because they share the
TUN address. Artifacts for each scenario are written to `target/e2e/<name>/`:
`system.log`, `llm-tape.log`, `transcript.jsonl`, `requests/` (every model
request body), and the isolated `state/` directory. `target/e2e/summary.json`
records each scenario's result and ordinary-heap high-water mark.

The harness answers the System's SNTP requests from the host clock through an
iptables DNAT rule, so time, Scheduler, and Workflow scenarios do not depend on
public NTP. Installing the rule needs root, through `sudo -n` when the harness
is not root; without it the run prints a warning and time-based scenarios may
fail. The rule is removed when the run ends. Pass `--no-local-ntp` to use real
NTP instead.

`--shard K/N` runs every N-th selected scenario starting with the K-th, so N
parallel runs cover the set once. CI runs the whole set in four shards.

## Scenarios

Scenarios live in `scenarios/*.toml`.

```toml
name = "Time tool"

[model]
mode = "scripted"          # scripted | recorded | none

[[model.responses]]        # one per /chat/completions call, in order
tool_calls = [{ name = "tool_load", arguments = { group_id = "time" } }]

[[model.responses]]
tool_calls = [{ name = "time_now", arguments = {} }]

[[model.responses]]
request_contains = ['"utc"']   # the request answered here must contain this
text = "I checked the clock."

[[steps]]                  # one stdin line to the CLI per step
send = "What time is it?"
reply_contains = ["I checked the clock."]
reply_matches = ['clock\.$']
tool_contains = ['"utc":"20']
notice_contains = []       # CLI notices, such as a permission prompt
kinds = ["tool"]           # message roles that must appear in the turn
# tool_errors_allowed = true

[[http]]                   # direct WebServer request after startup
method = "GET"
path = "/api/wifi"
status = 200
body_contains = ['"station"']

[logs]
expect = ["Agent created session"]   # regexes that must appear
forbid = ['\bERROR\b']               # default: ERROR and panics
allow = []                           # exceptions to forbid
ready = ["UTC clock synchronized"]   # wait for these before the first step
await = ["VM run 1 finished"]        # wait for these after the chat
await_seconds = 30

[memory]
heap_high_water_max = 131072          # ordinary-heap budget in bytes
```

Plugin Tools are hidden groups: a scripted model must `tool_load` the group
(`file`, `http`, `time`, `vm`, `workflow`, `scheduler`, `gateway`, `websearch`)
before calling its Tools, exactly as a real model does. Argument strings may
use `${now}` or `${now+Ns}`, expanded to RFC3339 UTC milliseconds when the tape
is generated.

Every scenario also fails when a Tool result is `tool not found` or an
`{"error": ...}` object, when llm-tape rejects or misses a model call, or when
the System logs an `ERROR` or a panic.

Keep a scripted scenario below eight committed turns and well below the
compaction threshold (about 24 KB of transcript). After eight turns the Agent
extracts long-term memory, and a long transcript is summarized; both make a
non-streaming model call that a scripted tape cannot answer, so the call takes
the next scripted response and every later step shifts. A detached completion
(a VM run that ends after its turn, for example) starts a turn of its own and
counts too. Split longer cases into numbered files such as `edge-vm-output-2`.

### Memory

The host Platform counts the ordinary (internal-RAM) heap separately from bulk
memory, which on devices lives in PSRAM. It logs `ordinary heap high-water: N
bytes` as the peak grows, and every result line shows the scenario's peak.
`[memory] heap_high_water_max` fails a scenario whose peak exceeds the budget.
`run --heap-limit BYTES` instead caps the heap itself, so an allocation beyond
it aborts the System the way a device runs out of memory. Host-only
allocations (the I/O reactor, logger, and TUN driver) are counted too, so the
host figure is an upper bound on the device's.

### Scripted, recorded, and none

- `scripted` synthesizes an OpenAI-compatible streaming tape from
  `[[model.responses]]`; it is fully offline and deterministic.
- `recorded` replays `tapes/<scenario>.jsonl`. Record or refresh it against a
  live provider (request bodies and credentials are never stored):

  ```bash
  uv run --package barracuda-e2e barracuda-e2e run my-scenario --record \
    --env-file /path/to/llm.env
  ```

  The env file provides `BARRACUDA_LLM_BASE_URL`, `BARRACUDA_LLM_API_KEY`, and
  `BARRACUDA_LLM_MODEL`. Replay requires the Agent to make the same sequence
  of model calls, so keep recorded scenarios short and write tolerant
  assertions. Treat tapes as sensitive: responses are stored verbatim.
- `none` makes no model calls; use it for `[[http]]`-only scenarios.

### Direct

`run --direct --env-file FILE` runs `recorded` scenarios without a tape: the
System calls the provider itself, through its own TLS and trust roots, so this
is the end-to-end check of HTTPS. Nothing is recorded and the replay checks
are skipped.

A network that intercepts TLS (such as a sandbox egress gateway) presents its
own CA, which the System rightly rejects. `--test-roots PEM` builds the host
System to also trust the roots in that file; device firmware refuses the
option, and the build warns not to ship it:

```bash
uv run --package barracuda-e2e barracuda-e2e run live-chat --direct \
  --env-file /path/to/llm.env --test-roots /path/to/gateway-ca.pem
```

`provision.sh` clamps the TCP MSS of forwarded connections to the path MTU:
the System's TCP stack ignores ICMP "fragmentation needed", so without the
clamp a full-size segment vanishes when the outbound link's MTU is smaller
than the TUN's.
