# IMessage Gateway Plugin

- Plugin ID: `imessage-gateway`
- Direct dependency: `workflow`
- Provided capability: `IMessageGateway`
- Workflow Actions: `gateway.send`, `gateway.send_stream`, `gateway.send_media`
- Workflow Events: `gateway.message.received`,
  `gateway.send_stream.finished`, `gateway.send_media.finished`

`IMessageGateway` is the shared typed API. Provider Plugins register channels
and publish inbound messages through it; Workflow Actions and the separate
`agent-imessage-gateway` adapter call the same send methods. The Plugin owns four
text stream workers and four media workers.
