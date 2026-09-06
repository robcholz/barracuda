# Web Search Plugin

- Plugin ID: `web-search`
- Direct Plugin dependencies: `webserver`
- Provided typed capabilities: none
- Required typed capabilities: `WebServer`
- Owned Components: `WebSearchComponent`
- Owned tasks: none

The Web Search Plugin accepts Agent and Workflow search requests through one
public unary JSON RPC. Each caller awaits the Tavily request and receives the
bounded search result directly in the RPC response. The Component retains one
reusable HTTP workspace; a concurrent search receives `busy` instead of causing
another set of network buffers to be allocated.

The Plugin owns Tavily configuration, outbound HTTP requests, provider response
validation, and JSON response encoding. `POST /api/tavily` atomically replaces
the `api_base` and `api_key` entries in the Plugin's private KV scope, then
replaces the active in-memory configuration. The persisted configuration is
restored when the Plugin registers. An active search retains an `Rc` snapshot
of the configuration without cloning the API key. The API key is never logged
or returned.

The Component allocates HTTP header, read, and request buffers once. Its bounded
provider response buffer retains its high-water capacity between searches.
The query remains borrowed from the request lane for the duration of the call.
Tavily's complete JSON response is retained only until the same call writes its
response into the lane.
