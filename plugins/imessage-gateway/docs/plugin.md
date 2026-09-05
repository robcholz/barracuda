# IMessage Gateway Plugin

- Plugin ID: `imessage-gateway`
- Direct Plugin dependencies: none
- Provided typed capabilities: `IMessageGateway`
- Required typed capabilities: none
- Owned Components: `GatewayComponent`, `GatewayInboundComponent`, four
  `GatewayTextStreamComponent` workers, four `GatewayMediaStreamComponent` workers
- Plugin-owned tasks: none

The Plugin owns the provider-neutral message gateway. Provider Plugins require
`IMessageGateway`, register a typed `MessageChannel`, retain its registration
guard, and publish normalized inbound messages through `GatewayIngress`.
Dropping the guard unregisters that provider channel.

Agent and Workflow callers do not use this capability. They discover the three
public JSON RPCs with visibility `"*"` and receive asynchronous Gateway facts
as JSON Events.
