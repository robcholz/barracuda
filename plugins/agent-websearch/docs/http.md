# Agent Websearch Tavily configuration

## `POST /api/tavily`

Installs or replaces the durable Tavily configuration.

```json
{
  "api_key": "tvly-...",
  "api_base": "https://api.tavily.com"
}
```

`api_base` is optional and defaults to Tavily's production API origin. The
endpoint returns `204` on success, `400` for malformed JSON, `405` for other
methods, `422` for an empty, longer than 256-byte, or non-printable-ASCII API
key or an invalid, longer than 512-byte, or non-printable-ASCII HTTP(S) base
URL, and `500` when persistence fails. The bounds keep every search request,
with the longest query, inside the Plugin's fixed request buffer. The `api_base` and `api_key` entries are committed in
one private KV transaction before the active configuration changes. An active
search keeps using the configuration snapshot captured when its request began.
