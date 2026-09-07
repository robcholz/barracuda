# Agent Websearch Plugin

- Plugin ID: `agent-websearch`
- Direct Plugin dependencies: `agent`, `webserver`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry`, `WebServer`
- Owned Components: none
- Owned tasks: none

During registration the Plugin adds the awaited `web_search` Tool directly to
`AgentToolRegistry`. It does not expose an Event Router RPC or a Workflow
Action.

The Plugin owns Tavily configuration, outbound HTTP requests, provider response
validation, and Agent Tool response encoding. `POST /api/tavily` atomically
replaces the `api_base` and `api_key` entries in the Plugin's private KV scope,
then replaces the active in-memory configuration. The persisted configuration
is restored when the Plugin registers. An active search retains an `Rc`
snapshot of the configuration without cloning the API key. The API key is never
logged or returned.

One reusable HTTP workspace prevents repeated allocation of header, read, and
request buffers. A concurrent search receives `busy`. The provider response is
bounded at 64 KiB, but the old 512-byte Event Router response limit and its
result truncation are gone.
