# IMessage BlueBubble Plugin

- Plugin ID: `imessage-bluebubble`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`
- Provided typed capabilities: none

The Plugin starts without provider credentials. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/bluebubbles` for runtime configuration, and
registers the configured channel for its lifetime. See [`http.md`](http.md).
