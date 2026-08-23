# Gateway-Agent Plugin

- Plugin ID: `gateway-agent`
- Direct Plugin dependencies: `agent`, `message-gateway`

The Gateway-Agent Plugin loads the standalone Gateway-Agent bridge Component.
The bridge routes normalized inbound gateway Events into Agent session RPCs and
delivers Agent output through Message Gateway RPCs.

It is an integration mapping between the Agent and Message Gateway contracts;
it does not provide or require a typed Plugin capability.
