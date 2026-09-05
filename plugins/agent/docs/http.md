# Agent HTTP API

## `POST /api/model-api`

Configures one or more model APIs directly on the Agent runtime. The Agent
Plugin registers this endpoint on its required `WebServer` capability during
Plugin registration.

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

The array must not be empty and unknown fields are rejected. `backend` is
`openai_compatible` or `anthropic_compatible`. `purpose` is `root_agent`,
`sub_agent`, `memory`, or `compaction`.

| Status | Meaning |
| --- | --- |
| `204 No Content` | All configurations were accepted. |
| `400 Bad Request` | The body is not a valid, non-empty configuration array. |
| `405 Method Not Allowed` | The route was called with a method other than POST. |
| `422 Unprocessable Entity` | Agent rejected a model API configuration. |

Error responses are JSON objects with one stable `error` string. API keys are
never included in responses or logs.
