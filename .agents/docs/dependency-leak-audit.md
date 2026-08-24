# Dependency Leakage Audit

This document maps dependency leakage in the current Barracuda workspace. It
records migration evidence and ordering; it does not replace
[`platform-architecture.md`](platform-architecture.md) or define a second target
architecture.

## Leakage model

The current dependency pressure reaches consumers in three forms:

- **type leakage** carries an implementation choice through generic parameters
  and public return types;
- **runtime lookup leakage** hides construction edges behind a registry;
- **ownership leakage** makes one subsystem construct resources owned by a
  different subsystem.

A dependency is not a leak merely because it is generic, dynamically
dispatched, or supplied by an external crate. It becomes a leak when it crosses
the owner boundary and forces unrelated consumers to retain that implementation
choice.

## Leakage map

| Priority | Dependency path | Current spread | Ownership boundary |
| --- | --- | --- | --- |
| P0 | Board -> Platform | Target passes the complete selected `Board` into Platform initialization; concrete Platforms inspect chip identity and native Board layout | Target validates compatibility and constructs the two selected axes; Platform consumes only Platform-owned inputs |
| P0 | Platform -> System | Platform exposes `ip_stack` and `partitions`, while System still expects the former `network`, `filesystem`, and `database_region` shape | System consumes `TargetResources`, mounts storage from partitions, and routes Board HAL capabilities independently |
| P0 | reqwless -> Agent | `Tcp`, `Resolver`, and `ModelApiFactory` spread from model-api through Agent, Runtime, Session, and Component APIs | model-api owns reqwless transport state; Agent consumes model-domain operations |
| P1 | global filesystem -> Agent | Agent Plugin obtains the complete System filesystem and retains its concrete type throughout the Agent graph | System supplies the existing scoped or rooted filesystem view required by Agent; concrete storage types stop at construction boundaries |
| P1 | physical flash -> System type | `PluginManager<M, DatabaseRegion>` makes the physical `NorFlash` type part of the public `System` type | Plugin Manager owns its database implementation and exposes only `PluginStorage` to Plugins; System hides the retained database type from its caller |
| P1 | duplicate HTTP integration | `shared/http-client` and Agent model-api independently wrap reqwless with different request, response, buffering, and streaming models | Concrete protocol owners use one reqwless integration policy; adapters remain private to their owner |
| P1 | channel HTTP construction | Telegram, Wechat, and BlueBubbles accept an HTTP abstraction, but no production composition constructs these Plugins | Selected composition constructs enabled channel Providers with the concrete HTTP implementation |
| P2 | Embassy Stack -> WebServer and Time | WebServer owns TCP accept loops; Time owns DNS, UDP, and SNTP behavior | These protocol owners may use Embassy directly internally; the Stack does not continue into their business consumers |

## Current composition break

`PlatformResources` contains `ip_stack` and `partitions`. System now consumes
the complete `TargetResources`, preserves Board HAL ownership, and assigns the
`filesystem`, `database`, and `web-assets` partitions after validating their
access disciplines.

The focused check

~~~text
cargo check -p barracuda-system
~~~

now type-checks across the selected Target and CLI boundary. System construction
returns an explicit storage-not-constructed error after resource assignment;
mounting the selected partitions and restoring Plugin assembly remain the next
composition work. No test filesystem or discarded partition stands in for that
missing storage assembly.

## Platform and Board

The architecture defines Platform and Board as independently selected axes,
with compatibility validation owned by Target composition. The current code
reverses that dependency in several places:

- `Platform::Bindings` is documented as commonly containing `&'static Board`;
- selected Target composition passes `barracuda_board_selected::BOARD` directly
  into selected Platform initialization;
- macOS and Linux Platform implementations depend on the Board crate and
  reject Board chip names themselves;
- ESP32-C6 and STM32 Platform bindings accept `Board`, validate its chip, and
  receive an already initialized IP stack;
- concrete Platform build scripts parse selected Board configuration.

Target composition must own compatibility. Native Board layout remains the
authoritative layout source, but Platform receives the concrete native artifact
or assigned mechanism it needs rather than the complete Board model. Platform
continues to own construction and runner lifecycle for its IP service.

## System and Plugin Manager

Plugin Manager contains provider-qualified Plugin capabilities declared through
`Plugin::DEPENDS_ON`. This path expresses Plugin graph ownership and supports
registration rollback and unload. System supplies fixed construction inputs
directly when it constructs each concrete Plugin.

The Embassy task spawner is a specific lifecycle facility rather than an
arbitrary capability lookup. It may remain an explicit startup-context field.

Plugin Manager correctly exposes semantic `PluginStorage` to Plugin code.
Internally it retains a concrete `NorFlash` database, but this physical type
also parameterizes `PluginManager` and consequently the public `System` type.
That retained implementation type should be contained at the System boundary.

## Network and HTTP

Barracuda already uses the relevant ecosystem implementations:

- Embassy Net owns IP, TCP, UDP, and DNS;
- `embedded-nal-async` supplies transport contracts used by reqwless;
- reqwless implements the HTTP client;
- picoserve implements the HTTP server.

Barracuda does not need a second general network framework. Thin glue is
appropriate where these concrete APIs meet, but it remains private to the
protocol owner and is not promoted into a new `NetworkServices`, client handle,
listener framework, or System registry.

WebServer and Time are protocol owners. Their internal use of Embassy sockets
remains contained inside those implementations after System supplies the stack
as a direct construction input.

### Duplicate HTTP paths

The message-channel Providers use `shared/http-client`, whose public response
model buffers the complete body. Agent model-api owns a separate reqwless
transport because model streaming must retain and incrementally read an HTTP
response. The two paths duplicate HTTP construction and error translation but
do not provide equivalent behavior.

A shared public HTTP abstraction must not be introduced merely to merge these
files. Reqwless-specific allocation, connection reuse, TLS, and buffering stay
inside the concrete owners. Common code is extracted only after the owners have
the same proven contract.

## Agent

Agent is the largest active leak. Production networking types currently occur
across six Agent crates and 36 source files:

- `barracuda-model-api`;
- `barracuda-agent`;
- `barracuda-agent-session`;
- `barracuda-agent-runtime`;
- `barracuda-agent-component`;
- `barracuda-agent-plugin`.

`ModelApiFactory<Tcp, Resolver>` exists to create independent reqwless clients
with borrowed transport state and reusable buffers. That is a reqwless resource
management concern, but it appears in Agent managers, context providers,
approval handling, runtime workers, sessions, Components, and stream types.

The containment pattern already exists in Event Router: construction accepts a
generic filesystem, installs an owner-private Component, and returns an
`EventRouter` type that does not retain the filesystem parameter. Agent should
follow the same boundary. Model transport resources remain owned by model-api;
Agent runtime handles, sessions, Components, and event streams carry Agent
domain types only.

Agent storage has a similar but smaller issue. Persistence, memory, skills, and
sandbox implementations legitimately operate on a filesystem. The complete
System filesystem and its type do not need to remain visible in the public
Agent Plugin, Runtime, Session, and Component graph after those stores are
constructed.

## Gateway and channels

The Gateway path is comparatively contained:

- `MessageChannel` is the intended heterogeneous Provider boundary;
- Gateway Agent and iMessage Web use Gateway, Agent, and WebServer semantic
  capabilities rather than Embassy network types;
- WebServer endpoints exchange owned HTTP and WebSocket domain values rather
  than Platform socket types.

The channel gap is composition rather than type leakage. Telegram, Wechat, and
BlueBubbles Plugins accept an HTTP dependency, but production System
composition currently registers only iMessage Gateway and the Web Provider.
No production call site constructs the external channel Plugins.

## Dependencies that remain valid

The following mechanisms have an owner-aligned reason to remain:

- Event Router's erased RPC handlers and method `TypeId` values support its
  heterogeneous RPC registry;
- provider-qualified Plugin capabilities support a dynamic Plugin graph with
  declared dependencies and lifecycle cleanup;
- Gateway's `MessageChannel` trait object supports runtime channel selection;
- WebServer's endpoint trait objects support heterogeneous routes;
- storage implementations may remain generic over their concrete filesystem
  or flash backend internally;
- `platform-test` dependencies used only by tests do not create production
  Platform coupling.

## Target dependency flow

~~~text
selected Platform --------------------+
                                       |
selected Board -> Drivers -> Board HAL +--> TargetResources
                                               |
                                               v
                                             System
                         +---------------------+--------------------+
                         |                     |                    |
                    Event Router         Plugin Manager       fixed Plugins
                    scoped storage       scoped key/value     direct inputs

ip_stack use inside protocol owners:

System composition
    +-- WebServer implementation -> picoserve + Embassy TCP
    +-- Time implementation      -> SNTP + Embassy DNS/UDP
    +-- model-api implementation -> reqwless transport
    +-- channel implementation   -> reqwless transport
~~~

The concrete IP stack appears at System composition and inside protocol-owner
implementation modules. It does not appear in Agent, Gateway, channel-domain,
or unrelated Plugin APIs.

## Migration order

1. **Restore Target -> System composition.** Make System consume the current
   `TargetResources`, assign partition roles, construct storage, and restore the
   focused System build.
2. **Contain Agent transport.** Stop exposing `Tcp`, `Resolver`, reqwless, and
   `ModelApiFactory` beyond model-api. Apply the Event Router construction
   pattern to Runtime, Session, Component, and stream types.
3. **Contain Agent storage.** Construct existing rooted/scoped storage at the
   System or Agent owner boundary and remove the complete System filesystem from
   Plugin lookup.
4. **Restore Platform/Board independence.** Move compatibility checks to Target,
   remove Board dependencies from Platform crates, and make Platform own IP
   construction and runner lifecycle.
5. **Resolve duplicate HTTP integration and channel composition.** Keep reqwless
   details private, remove unused duplicate public surface, and construct each
   selected channel in production composition.

Each migration step restores one boundary and its tests before beginning the
next. New general-purpose capability, network, storage, or transport
abstractions are outside this migration unless two concrete owners demonstrate
the same contract.

## Verification gates

The migration is complete when all of the following hold:

- `cargo check -p barracuda-system` passes;
- production source contains no `provide_system` or `require_system`;
- Platform crates do not depend on `barracuda-board` or parse selected Board
  configuration;
- selected Target composition is the only compatibility owner;
- Agent crates outside model-api contain no `TcpConnect`, `Dns`, reqwless, or
  `ModelApiFactory` types;
- Agent Plugin obtains no complete filesystem through a runtime registry;
- reqwless imports occur only in concrete HTTP implementation modules;
- Gateway and Agent RPC contracts contain only their domain data;
- enabled external iMessage channel Plugins have production construction sites.
