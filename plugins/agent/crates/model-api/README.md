# barracuda-model-api

LLM client: OpenAI- and Anthropic-compatible chat, structured JSON output, and
image inference over an Agent-owned persistent HTTP transport.

The standalone LLM surface can be reused independently of agent execution.

## Entry point

`ModelApi` owns one Agent HTTP client:

| Method | Request | Returns |
|---|---|---|
| `ModelApi::chat` | `ChatRequest` | `LlmResponse` (text + reasoning + tool calls) |
| `ModelApi::chat_json` | `ChatRequest` + `StaticOutputSchema` | `ChatJsonResponse` (parsed `T` + tool calls) |
| `ModelApi::infer_media` | `MediaRequest` | `String` (model text about the image) |
| `ModelApi::chat_stream` | `ChatRequest` | `ChatStream` of `ChatStreamEvent` values containing `barracuda_runtime_utils::stream::StreamPart`; the stream exclusively borrows the HTTP transport until Drop |

Both `openai_compatible` and `anthropic_compatible` backends are supported; the
crate converts the unified request shape (and tool definitions, tool-call /
tool-result roles, structured-output config) into each provider's wire format.

## HTTP is injected

System supplies `http_client::ClientFactory`, which contains only TCP, DNS,
and TLS construction resources. This crate builds reqwless on top and owns the
persistent connection, request/response representation, reusable buffers, and
streaming behavior. Sequential calls on one `ModelApi` reuse its connection
when the server permits it.

## Cancellation

Calls take `Cancel`, a borrowed atomic cancellation token. For streaming calls
the token covers request send,
response headers, and body reads; dropping `ChatStream` also cancels body
transfer. Cancellation is non-retryable and surfaces as `Error::Cancelled`.

## Retries

Retry is configured **per call** via `RetryPolicy` on the request (not on the
client). A fresh request carries `RetryPolicy::default()` (2 retries, 500 ms
initial interval, exponential, capped at 8 s); override with `.with_retry(..)`
or disable with `RetryPolicy::none()`. Only transient transport failures are
retried (network errors and HTTP 408/429/5xx); aborts, bad URLs/bodies, and
other 4xx are never retried. See `Error::is_retryable` for the
classification.

## Public API

Curated re-exports (implementation modules — backend registry, media-prep
pipeline, retry loop — are private):

- Client: `ModelApi`, `ModelApiFactory`
- Config / requests: `ModelApiConfig`, `BackendKind`, `ChatRequest`,
  `MediaRequest`, `RetryPolicy`, `StaticOutputSchema`
- Responses / values: `LlmResponse`, `ChatJsonResponse`, `ChatStream`,
  `ChatStreamEvent`, `ToolCall`, `MediaAsset`
- Errors: `Error`, `InitError`, `ParseBackendKindError`

## Example

```bash
cargo run -p barracuda-model-api --example client
```

Builds the same persistent transport over a scripted network factory and runs a
chat.

## Where it fits

A no_std core crate depending on `http-client`, `embassy-time`,
`barracuda-runtime-utils`, `serde`/`serde_json`, `base64`, and `thiserror`. It is
consumed by `barracuda-agent-runtime` and the memory-side LLM providers.
