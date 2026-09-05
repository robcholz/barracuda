# File Plugin JSON RPC

Both methods are unary JSON RPCs with visibility `"*"`. Paths and file contents
are UTF-8. Paths contain 1 through 255 bytes and resolve inside the File
Plugin's private filesystem namespace. File contents are limited to 240 UTF-8
bytes.

## `file.read`

- Request schema: `schemas/rpc/read/request.json`
- Response schema: `schemas/rpc/read/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 512 bytes

Request:

```json
{"path":"state/notes.txt"}
```

Success:

```json
{"content":"first line\nsecond line"}
```

## `file.write`

- Request schema: `schemas/rpc/write/request.json`
- Response schema: `schemas/rpc/write/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 29 bytes

Request:

```json
{"path":"state/notes.txt","content":"first line\nsecond line"}
```

Success has no result payload:

```json
{}
```

The path and decoded content retain their independent 255-byte and 240-byte
business limits. The complete encoded request must also fit the shared 512-byte
RPC lane. JSON escapes are decoded before writing, so newlines, quotes,
backslashes, and Unicode escapes behave as normal JSON strings.

## Business errors

Both methods may return:

```json
{"error":"invalid_request"}
{"error":"not_found"}
{"error":"permission_denied"}
{"error":"too_large"}
{"error":"io"}
```

`file.read` may additionally return:

```json
{"error":"invalid_utf8"}
```

`invalid_request` covers semantically invalid paths. `too_large` covers writes
or stored files above 240 decoded bytes, and read responses whose JSON-escaped
form cannot fit the response lane. Malformed JSON, a request exceeding the
512-byte transport bound, lane exhaustion, and other Event Router framing
failures are `RpcError` values rather than business documents.
