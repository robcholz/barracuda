# VM HTTP Plugin

The VM HTTP Plugin exposes the HTTP Plugin's bounded outbound requests to Lua
code as the require-only `http` Lua package.

- Plugin ID: `vm-http`
- Dependencies: `http`, `vm`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `Http` from `http`, `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `http`
- Web: none
- Tasks: one cancellable bridge task fed by a two-entry request queue; the
  `Http` capability's two workspaces bound how many requests run at once
- Storage: none

This Plugin exposes the HTTP Plugin's bounded high-level operation as the
require-only Lua `http` package. Lua never receives the Platform IP stack, DNS
resolver, TCP sockets, TLS configuration, or host networking primitives.

## Lua API

~~~lua
local http = require("http")
local response, error = http.request({
    method = "GET",
    url = "https://example.com/data",
    headers = {
        { name = "accept", value = "application/json" },
    },
    body = "",
})
~~~

Supported methods are `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, and `HEAD`.
Success returns `{ status=<integer>, body=<UTF-8 string> }`. A bounded HTTP
failure returns `nil, <stable error code>`. Invalid Lua request shapes and a
revoked or unavailable adapter raise a Lua error.

The underlying HTTP capability enforces a 2 KiB URL, 32 headers totaling at
most 16 KiB, a 32 KiB request body, a 64 KiB UTF-8 response body, two reusable
concurrent workspaces, and a 30 second whole-request deadline. The package does
not expose raw sockets or a general network library.
