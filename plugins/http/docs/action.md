# HTTP Workflow Action

`http.request` accepts `method`, absolute `url`, optional ordered `headers`, and
an optional UTF-8 `body`. It returns the upstream numeric `status` and complete
UTF-8 response `body`.

The URL is limited to 2 KiB, the body to 32 KiB, and at most 32 headers may use
16 KiB in total. The complete request has a 30 second deadline and the response
body is limited to 64 KiB.

The client writes `Host`, `Content-Length` or `Transfer-Encoding`, and the
connection headers itself. A caller header with one of those names, a name that
is not an HTTP token, or a value with a control character other than tab is
rejected with `invalid_header`, so a request cannot be framed or routed twice.

Stable errors are `invalid_url`, `tls_not_configured`, `invalid_header`,
`request_too_large`, `busy`, `timeout`, `transport`, `invalid_response_text`,
and `response_too_large`.
