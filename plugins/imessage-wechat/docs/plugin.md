# IMessage WeChat Plugin

- Plugin ID: `imessage-wechat`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`
- Provided typed capabilities: none

The Plugin starts without provider credentials. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/wechat` for runtime configuration, and
registers the configured channel for its lifetime. See [`http.md`](http.md).
