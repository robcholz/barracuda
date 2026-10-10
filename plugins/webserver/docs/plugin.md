# WebServer Plugin

- Plugin ID: `webserver`
- Direct Plugin dependencies: none
- Provided typed capabilities: `WebServer`

The WebServer Plugin owns one portable picoserve server and one Embassy server
task. Its constructor takes the common `PluginContext` and copies the public
`ip_stack` handle; the task creates `TcpSocket` values and calls
`TcpSocket::accept(8787)` directly. The macOS and Linux Platforms assign the
stack `10.42.0.2`, so its endpoint is `10.42.0.2:8787`.

It provides the typed `WebServer` capability so dependent Plugins can register
resource providers with `serve`, WebSocket routes with `serve_websocket`,
ordinary HTTP routes with `serve_http`, and streaming request bodies with
`serve_upload`. All return scoped registrations that dependent Plugins retain
for their lifetime. Registration publishes the capability and installs every
route before startup. The WebServer startup hook then obtains the System-owned
Embassy spawner and starts the server task.
Plugin unload or startup rollback cancels the task and drops all pending
accept/connection futures.

Normal Platform initialization separately starts the Embassy Net runner. The
WebServer task concurrently polls four fixed connection workers, each with its
own TCP and HTTP buffers. A long-lived WebSocket therefore uses one worker
without blocking ordinary HTTP requests in the remaining workers.

`WebServer::listen_on_stack` lets a dependent network-owner Plugin register the
same route table on an additional Platform stack and port during registration.
The WebServer Plugin still owns and cancels the resulting listener task; the
dependent Plugin retains a `WebListenerRegistration` that stops that listener
when dropped. Capacity is one additional interface with one connection worker;
another registration fails during Plugin registration. The Wi-Fi
Plugin uses this contract for captive HTTP on its access-point stack.

Each worker's 4 KiB receive buffer, 4 KiB transmit buffer, and 8 KiB HTTP
buffer are POD-only `BulkBox<[u8]>` allocations. Platforms with a separate
bulk-memory domain may place those buffers in external memory, while socket
state, futures, synchronization, and other control data remain in the normal
allocator.

HTTP consumers may also register a subtree with `serve_http_prefix`. Exact
HTTP and WebSocket routes take precedence, then the longest matching prefix
wins. Prefixes respect path segments: `/assets` matches `/assets` and
`/assets/file.js`, but not `/assets-other`; `/assets/` matches descendants only.
`/` provides an HTTP fallback. Exact and prefix registrations at the same path
can coexist; dropping one guard removes only that registration. Every new
request, including requests on an existing keep-alive connection, uses the
current route table. An already executing handler or WebSocket may finish
using its retained endpoint; removing its registration does not cancel it.
`WebServer::new` preserves picoserve's close-after-response default;
`WebServer::from(config)` accepts an explicit picoserve connection policy.

For resources, use `webserver.serve("/assets/*", provider)`. Implement
`HttpProvider::serve(&self, path: &str)` as an async function returning
`HttpResponse`; return `HttpResponse::stream(200, mime, length, reader)` for
streaming. The reader implements `embedded_io_async::Read` (0.7), whether its
source is a file, flash bytes, or generated content. No filesystem dependency
is added to WebServer. The provider opens its source once and transfers its
ownership to the response; the connection worker reads it under socket
backpressure. Missing resources can return an ordinary 404 response.

`serve` accepts exact paths or a single trailing `/*`. `/assets/*` matches
`/assets/` and descendants, not `/assets` or `/assets-other`. `/*` is the root
fallback. This resource entry accepts GET only (other methods receive 405),
passes the encoded path by reference without its query, and does not collect
the request body. The older `serve_http` interface remains buffered and owned.

A request body larger than the 8 KiB HTTP buffer needs `serve_upload`, which
registers a prefix route like `serve_http_prefix` and hands the endpoint an
`HttpUpload`: the method, the encoded path without its query, the declared
Content-Length, and the body as an `embedded_io_async::Read` (0.7) source
read straight from the socket. The endpoint may stop reading at any point and
answer; the server discards the unread remainder before writing the response,
so the connection stays usable. Upload endpoints accept every method and
choose their own status codes.

`HttpRequest::path()` exposes the original encoded path without its query.
The server does not decode paths or assign filesystem meaning to them. A
consumer implementing URL-to-resource mapping must validate that mapping and
enforce its own resource scope. `HttpRequest::new` remains available for
synthetic requests without a path; `with_path` supplies one explicitly.

`HttpResponse::new` returns buffered bytes. `HttpResponse::stream` accepts an
owned `embedded_io_async::Read` (0.7) source and an exact byte length. The server
emits Content-Type and Content-Length, copies at most 1024 bytes at a time,
and never reads beyond the declared length. The source need not be a file and
the server does not depend on a filesystem implementation. Empty streams do
not read their source. `HttpResponse::body()` now returns `Option<&[u8]>`:
`Some` for buffered bodies, `None` for streams.

The streaming adapter uses inline storage, not boxed readers or boxed read
futures. Its capacities are 16 machine words for the reader, 64 words for a
read future, and 256 words for a provider future, with machine-word alignment.
Oversized or over-aligned implementations fail at compile time; there is no
heap fallback. On 32-bit targets those capacities are 64, 256, and 1024 bytes.
The copy buffer is a separate 1024 bytes per active stream. These are storage
capacities, not a measurement of the entire Embassy worker's memory footprint.
An aggregator may retain `HttpProviderHandle` values for its leaf providers:
those futures have a 128-word inline budget, leaving room in the 256-word outer
handler for provider selection and ownership. The handle allocates only at
construction and does not register an additional HTTP endpoint.
No extra channel or task is created for a streaming connection. Registration
still allocates the route table and provider ownership.

`stream_allocations.rs` counts zero allocations for the complete picoserve
request/response path using a 40,000-byte generated resource, a yielding reader,
and a bounded short-writing test socket. This does not claim that an arbitrary
provider or the actual network stack allocates nothing. In particular the
current VFS `BackendFuture` is boxed, so a VFS-backed provider still incurs
VFS-owned allocations. That separate API is not changed here.

If a source returns an I/O error, premature EOF, or an invalid read length,
`serve_connection` returns `ServeConnectionError::ResponseSource` and drops
the response and socket. A partially transmitted response is not replaced
with another HTTP response or reused for a subsequent request. Socket write
errors continue to propagate as connection errors. Streaming is driven by
the existing connection worker and introduces no additional task.
