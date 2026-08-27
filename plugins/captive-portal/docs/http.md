# Captive Portal HTTP API

## `POST /api/model-api`

Configures one or more model APIs through the Agent Plugin's typed `AgentSetApi`
capability. The JSON request type belongs to this HTTP boundary; Agent receives
normal `ModelApiConfig` and `ApiPurpose` values for each array item.

Request body:

```json
[
  {
    "timeout_ms": 30000,
    "max_tokens": 4096,
    "image_max_bytes": 1048576,
    "backend": "openai_compatible",
    "purpose": "root_agent",
    "default": true,
    "api_key": "provider-secret",
    "model": "model-name",
    "base_url": "https://provider.example/v1"
  }
]
```

The array must not be empty. `backend` is `openai_compatible` or
`anthropic_compatible`. `purpose` is `root_agent`, `sub_agent`, `memory`, or
`compaction`.

Responses:

| Status | Meaning |
| --- | --- |
| `204 No Content` | All configurations were set successfully. |
| `400 Bad Request` | Body is not a valid, non-empty configuration array. |
| `405 Method Not Allowed` | Route was called with a method other than POST. |
| `422 Unprocessable Entity` | Agent rejected the model API configuration. |

Error responses are JSON objects with one stable `error` string. API keys are
never included in responses.
