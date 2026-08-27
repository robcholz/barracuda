# File Plugin RPC

The File Component exposes a deliberately bounded dynamic RPC harness. Both
methods are unary and carry `#[rpc_dynamic]` metadata.

## `file.read`

- Request: `FileReadRequest { path: FilePath }`
- Response: `FileBytes` (at most 240 opaque bytes)
- Errors: `InvalidRequest`, `NotFound`, `PermissionDenied`, `TooLarge`, `Io`

## `file.write`

- Request: `FileWriteRequest { path: FilePath, content: FileBytes }`
- Response: `()`
- Errors: `InvalidRequest`, `NotFound`, `PermissionDenied`, `TooLarge`, `Io`

Paths are UTF-8 and at most 255 bytes. They resolve inside the File Plugin's
`/system` scope; `/skills/...` therefore reaches the mounted skills alias.
