# WebServer Plugin

- Plugin ID: `webserver`
- Direct Plugin dependencies: none
- Provided typed capabilities: `WebServer`

The WebServer Plugin owns one portable picoserve server and one Embassy server
task. System passes the common `embassy_net::Stack` into the Plugin constructor;
the task creates `TcpSocket` values and calls `TcpSocket::accept(8787)` directly.
The macOS and Linux Platforms assigns the stack `10.42.0.2`, so its endpoint is
`10.42.0.2:8787`.

It provides the typed `WebServer` capability so dependent Plugins can register
portable WebSocket routes with `serve` and ordinary HTTP routes with
`serve_http`. Both return scoped registrations that dependent Plugins retain
for their lifetime. Registration publishes the capability and installs every
route before startup. The WebServer startup hook then obtains the System-owned
Embassy spawner and starts the server task; Event Router does not poll it.

Normal Platform initialization separately starts the Embassy Net runner. The
WebServer task concurrently polls four fixed connection workers, each with its
own TCP and HTTP buffers. A long-lived WebSocket therefore uses one worker
without blocking ordinary HTTP requests in the remaining workers.
