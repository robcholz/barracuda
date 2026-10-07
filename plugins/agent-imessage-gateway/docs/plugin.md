# Agent IMessage Gateway Plugin

`agent-imessage-gateway` depends on `agent`, `imessage-gateway`, and `workflow`.
During Plugin registration it adds the awaited `gateway_send` and
`gateway_send_media` Tools to `AgentToolRegistry` and registers
`imessage_bridge.to_agent` plus `imessage_bridge.to_gateway` in
`WorkflowActionRegistry`.

The bridge uses this Plugin's scoped key-value storage for the durable Gateway
route to Agent session mapping. Pending inbound replies and the active turn
reply remain runtime-only FIFO state. Action registration guards are retained
for the Plugin lifetime. The Action request and response contracts are in
`schemas/action`.

The Plugin owns no channel, streaming runtime, or background task.
