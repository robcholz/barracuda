# Captive Portal Plugin

- Plugin ID: `captive-portal`
- Direct Plugin dependencies: `webserver`
- Provided typed capabilities: `CaptivePortal`
- Required typed capabilities: `WebServer`
- Workflow Actions, Events, and Agent Tools: none

This plugin owns web entry aggregation and the portal scaffold resource entry.
It registers one GET subtree, `/portal/*`, with WebServer. It owns no additional
task, socket, or DNS service. It does not yet implement
OS captive-network detection, DNS interception, or automatic redirects.
The existing IMessage Web WebSocket at `/` is unchanged.

## Consumer contract

A plugin that contributes UI declares `captive-portal` as a dependency, obtains
`CaptivePortal`, and calls `register(WebEntry { id, title, module }, assets)`.
It retains the returned `WebEntryRegistration` in its registration context.
`AssetsProvider` is the existing WebServer `HttpProvider` trait, re-exported
under the portal's resource role; no second reader or transport contract exists.
The provider receives a validated relative path such as `chunks/main.js` and
returns an `HttpResponse`, normally `HttpResponse::stream(...)`.

For files, pass `ResourceFiles::from(context.filesystem()?.clone())` as the
provider. The contributing plugin declares `PluginFilesystem::Private`; the
adapter exposes only its `/resources` subtree. It shares the same implementation
as the portal scaffold provider, so consumers need no custom HTTP file handler.
Missing files return 404, an unmounted resource volume returns 503, and other
storage failures return 500 without disclosing filesystem details.

The two contributions are explicit: navigation metadata plus its provider.
Having a dependency on WebServer does not imply that a plugin has a web entry.
The portal does not inspect or derive domain UI from dependency metadata.
Consumers do not register individual web routes, and portal dependencies do
not grow as new UI contributors are added.

IDs follow `^[a-z0-9]+(?:-[a-z0-9]+)*$` and are limited to 64 bytes.
Module and resource paths are relative, at most 256 bytes, and use ASCII
letters, digits, `/`, `-`, `_`, and `.`. Empty, dot, and parent segments,
backslashes, percent escapes, and query characters are rejected. Generated
asset filenames should follow these constraints; URL queries are removed by
WebServer before dispatch.

## HTTP surface

- `/portal/entries.json` returns an array of `{id, title, module}`. Module URLs
  have the form `/portal/assets/<id>/<relative-module>`. Labels are JSON escaped.
- `/portal/assets/<id>/<path>` dispatches only to that registered provider.
- `/portal/` serves the portal's own `/resources/index.html`.
- Other `/portal/<path>` URLs serve the portal's own `/resources/<path>`;
  `entries.json` and the `assets/` subtree are reserved for aggregation.

The manifest is rebuilt when registrations change, not on HTTP requests.
It is held as a shared immutable byte snapshot. Registration and removal may
allocate; cached manifest lookup and leaf dispatch do not. WebServer's inline
leaf-provider handle reserves 128 machine words for each leaf future, leaving
room in the server handler for aggregation. Capacity violations fail to compile.
The normal source and socket copy buffers still apply: this is not zero-copy.
Filesystem-backed responses still incur the current VFS's own allocations.

Dropping a registration removes its entry from subsequent manifest responses
and removes subsequent access to its assets. Already active responses may
finish using their retained source or manifest snapshot. Reload can reuse the
same ID. A browser must fetch the manifest again to observe runtime changes;
no push notification or browser refresh policy is chosen here.

## Files and frontend scope

The plugin requests its own `ScopedVfs` and reads only `/resources`. It never
mounts a backend or opens another plugin's namespace. Each contributing plugin
owns its own asset provider and file scope.

Future scaffold output belongs in
`plugins/captive-portal/filesystem/resources/`; contributor output belongs in
`plugins/<id>/filesystem/resources/`. The generic image packaging step remains
responsible for including only enabled plugin contributions. No web-specific
packager, cross-plugin Bun bundle, or `include_bytes!` asset embedding is added.

The scaffold is implemented in `web/src/` using TypeScript, semantic HTML, and
CSS without a browser runtime framework or external fonts/CDNs. Bun emits
`index.html`, `app.js`, and `app.css` into `filesystem/resources/`. Build tools
and test dependencies remain in `web/`, outside the packaged resource tree.
A mounted volume without the scaffold
returns 404; an absent resource volume returns 503. System currently mounts
the durable `/data` volume; provisioning and mounting `/resources` remains a
System/image-integration requirement, not something this plugin performs.
The page renders the manifest as navigation and overview cards, imports a
module only when selected, and returns to the overview when the active entry
disappears. It refreshes on request, on becoming visible, and every 30 seconds
while visible. A failed refresh preserves the last known entries and displays
a connection error. Build-time exclusion
and runtime route removal are distinct: runtime unload does not erase image files.

## Bun build and module contract

From `plugins/captive-portal/web/`:

```sh
bun install --frozen-lockfile
bun run check
bun run test
bun run build
bun run dev
```

`dev` serves the built files on loopback and prints its URL (set `PORT` to
choose a port). Its manifest is deliberately empty; it does not pretend that
business plugins are connected. Device integration still uses the real Rust
manifest. Rebuild and refresh the preview after source changes.

Each contributor builds its entry independently as browser ESM into its own
`filesystem/resources/`, and registers that output path in `WebEntry.module`.
Do not statically import plugin modules into the scaffold. A module exports:

```ts
export function mount(root: HTMLElement, { signal }: { signal: AbortSignal }) {
  const label = document.createElement("p");
  label.textContent = "Module content";
  root.append(label);
  // Pass signal to fetch/listeners; clean up timers and other owned resources.
  return () => root.replaceChildren();
}
```

`mount` may return a promise of its cleanup function, or no cleanup function.
The shell aborts the signal and runs cleanup on navigation, unload, or page
exit. Every mount gets its own DOM root; stale imports cannot mount into a
new screen. Modules must handle cancellation and avoid modifying the shell's
global DOM. Modules are trusted same-origin plugin code, not sandboxed third
party content. Load module CSS inside the contributor (and remove owned styles
on cleanup); the shell does not discover extra CSS files. Relative imports
resolve from the contributor's resource URL. Prefer content-hashed module
filenames across firmware versions; JavaScript module caching is browser-owned.

## Verification

Tests cover entry validation, duplicate IDs, namespace separation, resource
path traversal rejection, cached dispatch allocation counts, actual HTTP
manifest JSON escaping, scaffold file streaming, and Plugin Manager unload/reload
removing/restoring the manifest and asset access together.
