# Tape replay heap profile (2026-08-26)

## Workload

`overall-capabilities.jsonl` contains 11 recorded responses and 1,212 raw
response chunks. The `tape-replay` scenario reconstructed all responses and
fed them through the production OpenAI-compatible streaming stack using
seven-byte reads. This intentionally represents a fragmentation/churn stress
case rather than typical socket coalescing.

## DHAT summary

| Metric | Value |
| --- | ---: |
| Total allocations | 394,897 |
| Total allocated bytes | 21,027,390 |
| Peak live allocations | 34 |
| Peak live bytes | 3,260,587 |
| Retained allocations | 41 |
| Retained bytes | 1,799,905 |

The dominant high-frequency sites were byte copies in the scripted transport
(164,988 blocks), `eventsource-stream`'s UTF-8 accumulator growth (164,968
blocks), and its event parser's `String::split_off` (25,592 blocks). The first
is profiling-fixture overhead; the latter two show that very small network
reads amplify allocate/free churn in the SSE layer.

The largest allocation-volume sites were UTF-8 accumulation (3.38 MiB),
response-vector construction (3.16 MiB), and SSE event splitting (2.88 MiB).
Loading the tape itself temporarily allocated 1.67 MiB. At the end of the run,
1.58 MiB remained in the reconstructed response bodies and about 204 KiB in
the scripted network's retained request capture. Those retained bytes belong
to the harness and must not be interpreted as production Agent leaks.

## Follow-up

Profile again with realistic socket read sizes before changing production
code. If SSE churn remains prominent, first investigate bounded reusable
buffers in the UTF-8/event boundary rather than optimizing the Agent session
layer. Keep tape loading and response construction outside the DHAT scope when
measuring only steady-state parsing.
