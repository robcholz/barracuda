# Tavily HTTP configuration

## `POST /api/tavily`

Installs or replaces the in-memory Tavily configuration.

```json
{
  "api_key": "tvly-...",
  "api_base": "https://api.tavily.com"
}
```

`api_base` is optional and defaults to Tavily's production API origin. The
endpoint returns `204` on success, `400` for malformed JSON, `405` for other
methods, and `422` for an empty API key or invalid HTTP(S) base URL.
