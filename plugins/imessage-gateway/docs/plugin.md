# IMessage Gateway Plugin

- Plugin ID: `imessage-gateway`
- Direct Plugin dependencies: none

The base Plugin loads the standalone IMessage Gateway Component and provides
the typed `IMessageGateway` capability.

Provider Plugins require that capability during startup, register one
`MessageChannel`, and retain the returned registration guard. Unloading a
provider drops its guard and unregisters its channel while the base Gateway
Component remains loaded.

Built-in provider Plugins:

- `imessage-web`
- `imessage-telegram`
- `imessage-wechat`
- `imessage-bluebubble`
