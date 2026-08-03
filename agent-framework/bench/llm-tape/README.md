# llm-tape

`llm-tape` records the raw response-body byte chunks and timing observed by a
local reverse proxy, then serves those bytes back to an agent through the same
LLM API URL shape. It does not parse SSE, JSON, model output, or provider event
types.

The current scope is intentionally narrow:

- reverse proxy recording through a configurable LLM base URL;
- one append-only JSONL tape per run;
- sequential request matching by HTTP method and path;
- replay with the original recorded timing only;
- no live-network fallback in replay mode.

## Install

From the `agent-framework` root, install every Python tool and development
dependency into the shared workspace environment:

```bash
uv sync --all-packages --all-groups
```

## Record

Start the recorder with the real provider as its upstream:

```bash
uv run --package llm-tape llm-tape record \
  --listen 0.0.0.0:8787 \
  --upstream https://api.anthropic.com \
  --output bench/tapes/run.jsonl
```

Point the agent at the recorder. An agent running on another machine or device
must use the recorder host's LAN address rather than `127.0.0.1`:

```bash
export ANTHROPIC_BASE_URL=http://192.168.1.10:8787
```

For an OpenAI-compatible client whose configured base URL includes `/v1`:

```bash
export OPENAI_BASE_URL=http://192.168.1.10:8787/v1
```

The recorder forwards every non-control path to the configured upstream. It
stores request metadata and a request-body SHA-256 for diagnostics, but never
stores the request body or authentication header values.

## Replay

```bash
uv run --package llm-tape llm-tape replay \
  --listen 0.0.0.0:8787 \
  bench/tapes/run.jsonl
```

Point the agent at the replay server using the same base URL. Incoming requests
consume recorded interactions in order. A method or path mismatch returns HTTP
409 without consuming the interaction. Exhausting the tape also returns 409.

Replay has no speed option: response headers and chunks are always scheduled at
their original monotonic offsets from the start of the matching request.

## Logs

`llm-tape` uses [Loguru](https://loguru.readthedocs.io/). Logs are colored when
stderr is an interactive terminal and automatically become plain text when
redirected. `INFO` is the default and reports server startup, request matching,
response status, chunk/byte totals, elapsed time, stream outcome, and failures.
It never logs request bodies, response contents, or authentication header
values.

Use `DEBUG` to additionally report the sequence, byte count, and original
timestamp of every recorded or replayed byte chunk:

```bash
uv run --package llm-tape llm-tape replay \
  --log-level DEBUG \
  --listen 0.0.0.0:8787 \
  bench/tapes/run.jsonl
```

## Tape format

The tape is append-only JSONL. Response data is base64 encoded so arbitrary
bytes, including splits inside JSON tokens or UTF-8 code points, round-trip
without interpretation.

```jsonl
{"kind":"tape_start","version":1,"created_at":"2026-08-03T08:00:00+00:00"}
{"kind":"request","interaction_id":"call-000000","call_index":0,"method":"POST","path":"/v1/messages","path_qs":"/v1/messages","headers":[["content-type","application/json"]],"body_sha256":"...","body_size":123}
{"kind":"response_start","interaction_id":"call-000000","at_us":120000,"status":200,"reason":"OK","headers":[["content-type","text/event-stream"]]}
{"kind":"response_chunk","interaction_id":"call-000000","seq":0,"at_us":135000,"data_b64":"ZGF0YToge30K..."}
{"kind":"response_end","interaction_id":"call-000000","at_us":180000,"outcome":"eof"}
```

Treat tapes as sensitive. Request credentials are redacted, but provider
responses are recorded verbatim and may contain private model context.

## Control endpoint

Both modes expose `GET /_llm_tape/health`. This endpoint is never recorded and
does not consume a replay interaction.

## Verification

```bash
uv run --package llm-tape ruff check bench/llm-tape
uv run --package llm-tape ruff format --check bench/llm-tape
uv run --package llm-tape \
  pytest -c bench/llm-tape/pyproject.toml bench/llm-tape/tests
```
