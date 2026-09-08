# HTTP Workflow Action

`http.request` accepts `method`, absolute `url`, optional ordered `headers`, and
an optional UTF-8 `body`. It returns the upstream numeric `status` and complete
UTF-8 response `body`.

The URL is limited to 2 KiB, the body to 32 KiB, and at most 32 headers may use
16 KiB in total. The complete request has a 30 second deadline and the response
body is limited to 64 KiB.

Stable errors are `invalid_url`, `tls_not_configured`, `invalid_header`,
`request_too_large`, `busy`, `timeout`, `transport`, `invalid_response_text`,
and `response_too_large`.
