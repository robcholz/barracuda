# Web Search JSON RPC

## `web_search.search`

- Visibility: `*`
- Transport shape: unary JSON request to unary JSON response
- Maximum request: 320 encoded bytes
- Maximum response: 512 encoded bytes
- Request schema: `schemas/rpc/search/request.json`
- Response schema: `schemas/rpc/search/response.json`

Request:

```json
{
  "query": "embedded Rust async executors",
  "max_results": 3
}
```

`query` must contain non-whitespace text and is limited to 255 decoded UTF-8
bytes. `max_results` is from 1 through 10. Unknown fields, the wrong JSON shape,
malformed JSON, and oversized documents are transport errors.

The call waits for Tavily and returns search results directly:

```json
{
  "results": [
    {
      "title": "Embassy executor",
      "url": "https://example.com/embassy",
      "content": "A bounded result excerpt...",
      "score": 0.91
    }
  ],
  "truncated": false
}
```

The response uses the complete 512-byte JSON document as one budget. Individual
fields do not have independent byte limits and URLs are never cut. If the next
complete result cannot fit, the response keeps the complete preceding results
and sets `truncated` to `true`. When a result's title, URL, and score fit, its
content may use the remaining response capacity. A result whose metadata cannot
fit without cutting a field is omitted.

Stable pre-acceptance business failures are:

```json
{ "error": "not_configured" }
```

```json
{ "error": "busy" }
```

`not_configured` means no valid Tavily configuration has been installed.
`busy` means the reusable HTTP workspace is occupied by another search.
Provider failures are returned by the same awaited call as `transport`,
`service`, or `invalid_response`.
