# HTTP RPC API

## `http.request`

- Address: `http.request`
- Dynamic JSON: yes
- Request: unary `HttpRequest`
- Response: unary `HttpResponse`
- Method error: `HttpRpcError`

`HttpRequest` contains an uppercase `method` (`GET`, `POST`, `PUT`, `PATCH`,
`DELETE`, or `HEAD`), an absolute `url`, two fixed header slots, and a UTF-8
`body`. Unused header slots have an empty name and empty value. URLs support
HTTP and HTTPS; HTTPS requires the TLS configuration supplied by the selected
Platform.

`HttpResponse` contains the numeric `status` and a buffered UTF-8 `body`.
URLs are limited to 192 bytes, header names to 32 bytes, header values to 64
bytes, request bodies to 96 bytes, response bodies to 508 bytes, and requests
carry two header slots. These bounds keep both DTOs within the Event Router's
512-byte lane frame.

Method errors:

| Variant | Meaning |
| --- | --- |
| `InvalidUrl` | The URL text is not canonical UTF-8, or the scheme is not `http`/`https`. |
| `TlsNotConfigured` | HTTPS was requested without Platform TLS. |
| `InvalidHeader` | A header name or value is not legal HTTP: empty name with a value, control characters or `:` in the name, CR/LF in the value, or undecodable wire text. |
| `Transport` | DNS, TCP, TLS, HTTP, or response-body reading failed. |
| `InvalidResponseText` | The response body is not UTF-8, or contains a NUL. |
| `ResponseTooLarge` | The response exceeds the fixed buffer. |
