# IMessage Gateway Runtime

The Plugin owns one `GatewayRuntime` shared by its typed capability, Workflow
Actions, and fixed workers:

- four workers concurrently drive semantic Agent event streams;
- four workers concurrently drive binary media streams;
- inbound provider messages are emitted directly through `WorkflowService`.

Each stream still enforces its sequence and bounded queue. These are runtime
backpressure rules, not Event Router lane limits. A slow provider stream does
not block another stream or inbound Workflow Event delivery.
