# Gateway-Agent Plugin

- Plugin ID: `gateway-agent`
- Direct Plugin dependencies: `agent`, `imessage-gateway`
- Provided typed capabilities: none

The Plugin loads the stateful Gateway-Agent Component. It does not depend on a
specific provider: Web, iMessage, WeChat, Telegram, and future channels all use
the route carried by `gateway.message.received`.

The Component installs the two-step Workflow documented in
[`component.md`](component.md). No Agent type crosses the Gateway/provider
boundary; `gateway_agent.respond` maps Agent events to generic Gateway stream
fields first.
