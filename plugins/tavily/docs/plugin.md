# Tavily Plugin

- Plugin ID: `tavily`
- Direct Plugin dependencies: `webserver`
- Provided typed capabilities: none
- Owned Components: `TavilyComponent`
- Required typed capabilities: `WebServer`

The Tavily Plugin gives Agents a bounded, dynamic web-search RPC backed by the
Tavily Search API. It owns credential configuration, outbound Tavily HTTP
requests, response validation, and conversion into fixed-layout result frames.
The API key is never logged or returned through either contract.

Configuration is installed at runtime with `POST /api/tavily`. The Component
returns `NotConfigured` until that endpoint has accepted a configuration.
