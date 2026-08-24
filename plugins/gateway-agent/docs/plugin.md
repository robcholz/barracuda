# Gateway-Agent Plugin

- Plugin ID: `gateway-agent`
- Direct Plugin dependencies: `agent`, `imessage-gateway`, `imessage-web`

The Gateway-Agent Plugin loads the standalone Gateway-Agent bridge Component.
The bridge routes normalized inbound gateway Events into Agent session RPCs and
delivers Agent output through IMessage Gateway RPCs.

It is an integration mapping between the Agent and IMessage Gateway contracts;
it requires `IMessageWebRoute` from `imessage-web` to select its reply route.
