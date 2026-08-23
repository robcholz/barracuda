# Message Gateway Plugin

- Plugin ID: `message-gateway`
- Direct Plugin dependencies: `webserver`

The Message Gateway Plugin constructs the gateway facade and built-in Web
channel, loads the standalone Message Gateway Component, and mounts the Web
channel on the shared server.

It requires the `WebServer` capability from the `webserver` Plugin. It owns the
Message Gateway Component and retains the Web route registration for unload.
