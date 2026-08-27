# IMessage QQ Plugin

- Plugin ID: `imessage-qq`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`
- Provided typed capabilities: none

The Plugin starts without provider credentials. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/qq` for runtime configuration,
and registers the configured QQ channel for its lifetime. See [`http.md`](http.md).
