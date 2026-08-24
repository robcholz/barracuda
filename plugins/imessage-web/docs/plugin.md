# IMessage Web Plugin

- Plugin ID: `imessage-web`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`

The Plugin requires the `IMessageGateway` capability and registers the `web`
message channel for its lifetime. It requires the `WebServer` capability,
mounts the WebSocket bridge, and publishes `IMessageWebRoute` for consumers of
the built-in Web conversation.
