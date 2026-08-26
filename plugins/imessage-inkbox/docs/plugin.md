# IMessage Inkbox Plugin

- Plugin ID: `imessage-inkbox`
- Direct Plugin dependencies: `imessage-gateway`
- Provided typed capabilities: none

The Plugin owns one configured Inkbox provider, requires the
`IMessageGateway` capability, and registers the `imessage` message channel for
its lifetime. It sends through the Inkbox iMessage API using an identity-scoped
API key and supports text, streamed text, media, tapbacks, and typing signals.
