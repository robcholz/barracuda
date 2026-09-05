# iMessage Bridge Plugin

The `imessage-bridge` Plugin owns one Event Router Component. It persists the
bidirectional Gateway route to Agent session mapping and exposes two public JSON
RPCs for Workflows.

## Communication

The Plugin declares no typed capabilities. Workflow integration uses the
`imessage_bridge.to_agent` and `imessage_bridge.to_gateway` JSON RPC contracts.
