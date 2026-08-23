# WebServer Plugin

- Plugin ID: `webserver`
- Direct Plugin dependencies: none

The WebServer Plugin owns one portable picoserve server and a listener
Component driven by Event Router. System supplies the platform listener
capability; the built-in Tokio adapter binds `127.0.0.1:8787`.

With the `embassy` feature, `embassy_net::Stack<'static>` implements the same
listener interface and accepts TCP port `8787` on the device network stack.

It provides the typed `WebServer` capability so dependent Plugins can register
routes. It owns the WebServer listener Component, which begins accepting
connections only after every Plugin has started.
