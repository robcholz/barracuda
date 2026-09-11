# VM WebServer Plugin

- Plugin ID: `vm-webserver`
- Direct Plugin dependencies: `vm`, `vm-filesystem`, `webserver`
- Required typed capabilities: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`, `barracuda_vm_filesystem_plugin::VmFileTransfer` from `vm-filesystem`, `barracuda_webserver_plugin::WebServer` from `webserver`
- Provided typed capabilities: none
- Workflow Actions: none
- Workflow Events: none
- Agent Tools: none
- Owned long-running tasks: none
- Retained registrations: the `webserver` Lua package and the `/vm` HTTP prefix route

This Plugin installs the require-only Lua `webserver` package. A Lua execution
claims one bounded mount below `/vm` while `webserver.serve` is running. The
existing WebServer connection workers deliver requests to that execution; the
adapter does not own a listener, socket, network stack, or permanent task.

~~~lua
local webserver = require("webserver")

webserver.serve({
    mount = "site",
    handler = function(request)
        if request.path == "/" then
            local file = assert(io.open("/data/index.html", "rb"))
            return webserver.file(file, "text/html")
        end
        return { status = 404, content_type = "text/plain", body = "not found" }
    end,
})
~~~

The example is served below `/vm/site`. Requests contain `method`, the path
relative to that mount, and a binary-safe `body`. Buffered responses contain
`status`, `content_type`, and `body`.

Supported response content types are `text/plain`, `text/html`, `text/css`,
`application/json`, `application/javascript`, `application/octet-stream`,
`application/wasm`, `image/svg+xml`, `image/png`, `image/jpeg`, `image/gif`,
`image/x-icon`, and `font/woff2`; the UTF-8 variants of plain text and HTML are
also accepted.

`webserver.file(file, content_type [, status])` consumes an existing readable
VM file returned by `io.open`. It does not accept or reopen a path. The file
therefore retains the VM filesystem's existing namespace, permissions, path
validation, and open-file capacity. When the returned response is accepted,
transfer closes the Lua handle and moves its remaining bytes into
`HttpResponse::stream`; the open-file lease is held until the response stream
is dropped. The VM execution retains the concrete file reader and serves it to
the WebServer through a bounded one-chunk proxy, so streaming does not require
every VFS backend file to be transferable between executor tasks.

At most four mounts may be active. A mount accepts one active handler call and
one queued request. Additional requests receive 503. Unknown mounts receive
404. Handler failures and invalid response documents receive 500. A handler
that does not produce a response within 30 seconds receives 504 and releases
its WebServer connection worker. An active file stream retains that mount's
single handler slot until the stream completes or is dropped. Mount names
contain only lowercase ASCII letters, digits, and internal hyphens and are at
most 64 bytes. Removing a VM execution removes its mount; Plugin revocation
wakes pending handlers and prevents new registrations.
