# HTTP Workflow Action

`http.request` accepts `method`, absolute `url`, optional ordered `headers`, and
an optional UTF-8 `body`. It returns the upstream numeric `status` and complete
UTF-8 response `body`.

Stable errors are `invalid_url`, `tls_not_configured`, `invalid_header`, `busy`,
`transport`, `invalid_response_text`, and `response_too_large`.
