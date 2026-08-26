# IMessage Telegram Plugin

- Plugin ID: `imessage-telegram`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`
- Provided typed capabilities: none

The Plugin starts without provider credentials. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/telegram` for runtime configuration, and
registers the configured channel for its lifetime. See [`http.md`](http.md).
