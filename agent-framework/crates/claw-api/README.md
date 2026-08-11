# claw-api

LLM client: OpenAI- and Anthropic-compatible chat, structured JSON output, and
image inference over reqwless.

Extracted from `claw_core::llm` into a standalone crate so the LLM surface can
be reused independently of the agent core (e.g. by `claw-memory`'s compactor and
the `cap_llm_inspect` capability).

## Entry point

`ClawApi` owns one long-lived reqwless client, persistent connection, and
reusable buffers:

| Method | Request | Returns |
|---|---|---|
| `ClawApi::chat` | `ChatRequest` | `LlmResponse` (text + reasoning + tool calls) |
| `ClawApi::chat_json` | `ChatJsonRequest` | `ChatJsonResponse` (parsed `T` + tool calls) |
| `ClawApi::infer_media` | `MediaRequest` | `String` (model text about the image) |
| `ClawApi::chat_stream` | `ChatRequest` | `ChatStream` of `ChatStreamEvent` values containing `claw_utils::stream::StreamPart`; the stream exclusively borrows the HTTP transport until Drop |

Both `openai_compatible` and `anthropic_compatible` backends are supported; the
crate converts the unified request shape (and tool definitions, tool-call /
tool-result roles, structured-output config) into each provider's wire format.

## Networking is injected

The application supplies an `embedded_nal_async::TcpConnect + Dns` stack
directly to `ClawApi`. Embassy and host applications use the same reqwless HTTP
code and differ only in their TCP/DNS HAL implementation. Sequential calls on
one `ClawApi` reuse one reqwless `HttpResource` connection.

## Cancellation

Calls take `Cancel`, a borrowed atomic cancellation token. For streaming calls
the token covers request send,
response headers, and body reads; dropping `ChatStream` also cancels body
transfer. An abort is non-retryable and surfaces as `ClawApiError::Transport`
containing `HttpError::Cancelled`.

## Retries

Retry is configured **per call** via `RetryPolicy` on the request (not on the
client). A fresh request carries `RetryPolicy::default()` (2 retries, 500 ms
initial interval, exponential, capped at 8 s); override with `.with_retry(..)`
or disable with `RetryPolicy::none()`. Only transient transport failures are
retried (network errors and HTTP 408/429/5xx); aborts, bad URLs/bodies, and
other 4xx are never retried. See `ClawApiError::is_retryable` for the
classification.

## Public API

Curated re-exports (implementation modules — backend registry, media-prep
pipeline, retry loop — are private):

- Client: `ClawApi`, `ClawApiFactory`
- Config / requests: `ClawApiConfig`, `BackendKind`, `ChatRequest`, `ChatJsonRequest`,
  `MediaRequest`, `RetryPolicy`, `StaticOutputSchema`
- Responses / values: `LlmResponse`, `ChatJsonResponse`, `ChatStream`,
  `ChatStreamEvent`, `ToolCall`, `MediaAsset`
- Errors: `ClawApiError`, `HttpError`, `ChatError`, `ChatJsonError`,
  `InferMediaError`, `InitError`, `ParseBackendKindError`

## Example

```bash
cargo run -p claw-api --example client
```

Builds a client over the wire-level scripted TCP/DNS stack and runs a chat.

## Where it fits

A no_std core crate depending on `claw-net`, `embassy-time`, `claw-utils`,
`serde`/`serde_json`, `base64`, and `thiserror`. It is consumed by `claw-core`
and the memory-side LLM providers.
