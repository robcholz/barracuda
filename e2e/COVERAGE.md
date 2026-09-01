# End-to-end Plugin coverage

Agent-visible Event Router tools satisfy all four conditions:

1. the RPC belongs to an Event Router group;
2. the RPC has a runtime `Dynamic` surface;
3. `Dynamic::schema()` is `Some(baked_schema)`.
4. request and response cardinality are both unary.

The E2E journey proves the complete Web Gateway → Agent → generated tool →
Event Router RPC path for every RPC that currently satisfies this contract:

| Plugin | Generated Agent tool | E2E assertion |
| --- | --- | --- |
| `http` | `rpc_4_http_request` | Agent loads the `http` group; request method, URL, headers and body reach the loopback fixture; status and body return to the Agent |
| `time` | `rpc_4_time_now` | Agent loads the `time` group; loopback DNS/SNTP synchronization returns calendar time |

The same journey covers Agent model configuration through `captive-portal`,
real TCP/WebSocket ingress and egress through `webserver` and `imessage-web`,
and the `gateway-agent` bridge into `agent`.

The remaining loaded Plugins are deliberately not presented as Agent tools
yet. Their Event Router contracts are typed-only, dynamic without a baked
schema, streaming, or absent. They need an Agent-oriented unary dynamic RPC DTO
and schema bake before they can be genuine Agent E2E coverage; the adapter must
not invent a parallel hardwired contract to claim coverage. In particular this
currently includes Web Search, file, scheduler, Gateway provider
delivery/media, VM/Lua hardware packages, and message queue operations.

Provider-specific methods that have no Event Router Agent contract remain in
their provider contract tests. `scheduler.triggered` Event delivery remains in
the Scheduler/Workflow integration suite until System publishes an end-user
notification workflow.
