# WebServer Plugin

- Plugin ID: `webserver`
- Direct Plugin dependencies: none
- Provided typed capabilities: `WebServer`

The WebServer Plugin owns one portable picoserve server and a listener
Component driven by Event Router. System supplies the platform listener
capability; the built-in Tokio adapter binds `127.0.0.1:8787`.

It provides the typed `WebServer` capability so dependent Plugins can register
portable WebSocket routes with `serve` and ordinary HTTP routes with
`serve_http`. Both return scoped registrations that dependent Plugins retain
for their lifetime. It owns the WebServer listener Component, which begins
accepting connections only after every Plugin has started. Normal Platform
initialization supplies an Embassy spawner, and the listener runs each
connection in a statically allocated four-slot Embassy task pool. A long-lived
WebSocket therefore uses one slot without blocking ordinary HTTP requests in
the remaining slots.
