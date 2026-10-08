# Wi-Fi Plugin

- Plugin ID: `wifi`
- Direct Plugin dependencies: `webserver`, `captive-portal`
- Required typed capabilities: `barracuda_webserver_plugin::WebServer` from
  `webserver`, `barracuda_captive_portal_plugin::CaptivePortal` from
  `captive-portal`
- Provided typed capability: `barracuda_wifi_plugin::WifiControl`
- Workflow Actions: none
- Workflow Events: none
- Agent Tools: none
- System resource: the selected Platform's concrete
  `barracuda_platform::WifiDevice`
- Owned long-running tasks: one station/AP policy task, one DHCP server task,
  and one captive DNS task
- Retained registrations: the Wi-Fi Portal entry, Wi-Fi HTTP routes, captive
  detection routes, and the AP-side WebServer listener
- Storage: station credentials under the Plugin-scoped KV key `station`

Dependent Plugins can require `WifiControl` from `wifi` to inspect support and
state, scan networks, start or stop the access point, configure a persisted
station connection, or forget it and return to the setup AP. The capability
owns these policy transitions, erases the selected Platform's concrete
`WifiDevice` type, and serializes storage and radio operations. The HTTP routes
are adapters over the same capability rather than a separate policy path.

The Plugin owns Wi-Fi policy rather than a specific radio driver. On a managed
embedded Platform it restores persisted station credentials during startup. A
successful restore leaves the device in station mode. Missing or unusable
credentials start the open `Barracuda Setup` access point at `192.168.4.1/24`.
The Plugin then supplies DHCP and captive DNS while the WebServer Plugin serves
the shared route table on AP port 80 with one connection worker.

`GET /api/wifi` returns capabilities plus current station and access-point
state. `GET /api/wifi/scan` returns nearby networks. `PUT /api/wifi` validates,
connects, and only then persists `{ "ssid", "password" }`; after the response,
the setup AP stops. A failed connection keeps or restores the setup AP so the
caller can retry. `DELETE /api/wifi` removes persisted credentials,
disconnects station mode, and starts the setup AP.

The Portal registers a Wi-Fi module and exact captive-detection paths used by
common Android, Apple, and Windows clients. Credentials are never returned by
the status API or logged. The provisioning API currently has no authentication
or transport encryption; the setup access point is intentionally open, so
provision only in a trusted physical environment.

Host Platforms use `HostWifiDevice`: AP and configuration operations are
unsupported no-ops and station state is already connected because networking
is managed by the host OS. Platforms without a Wi-Fi implementation report a
disconnected `UnavailableWifiDevice` instead of claiming host connectivity.

The selected ESP32-S3 implementation owns the ESP radio controller and both
Embassy network interfaces. Platform-owned runners drive station DHCP and the
static AP stack. The Wi-Fi Plugin owns only policy and provisioning services.
Radio state, socket state, futures, locks, and atomics remain in internal RAM.
WebServer TCP and HTTP byte buffers use `BulkBox<[u8]>`, allowing the selected
Platform to place those POD buffers in PSRAM safely.

Portal page: the `wifi` entry (group Device, order 10) is built on the
portal UI kit from `resources/web/entry.ts`. Its header shows the station
state, network and setup-hotspot state from `GET /api/wifi`, beside the
`router` figure (`resources/web/figure.js`), whose antennas sweep while
`GET /api/wifi/scan` runs. The page scans on open and on 重新扫描, lists
nearby networks strongest first with signal bars and a lock for secured
ones, joins a listed or manually named network with `PUT /api/wifi`, and
offers `DELETE /api/wifi` for the connected network. Phones get the design's
list layout. Results are reported as portal toasts, in Chinese or English.

Cargo automatically runs the declared `build` task before compiling this
Plugin. To rebuild only the Portal assets, run
`cargo plugin run --plugin wifi build` from the repository root. It writes
`filesystem/resources/entry.js`, `figure.js` and `icon.svg` (the Lucide
`wifi` icon), bundled as the Plugin's private `/resources/` files.
