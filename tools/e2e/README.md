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
request body), and the isolated `state/` directory.

The harness answers the System's SNTP requests from the host clock through an
iptables DNAT rule, so time, Scheduler, and Workflow scenarios do not depend on
public NTP. It needs root; without it the run prints a warning and time-based
scenarios may fail. The rule is removed when the run ends. Pass
`--no-local-ntp` to use real NTP instead.

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
await = ["VM run 1 finished"]        # wait for these after the chat
await_seconds = 30
```

Plugin Tools are hidden groups: a scripted model must `tool_load` the group
(`file`, `http`, `time`, `vm`, `workflow`, `scheduler`, `gateway`, `websearch`)
before calling its Tools, exactly as a real model does. Argument strings may
use `${now}` or `${now+Ns}`, expanded to RFC3339 UTC milliseconds when the tape
is generated.

Every scenario also fails when a Tool result is `tool not found` or an
`{"error": ...}` object, when llm-tape rejects or misses a model call, or when
the System logs an `ERROR` or a panic.

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
