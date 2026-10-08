# Captive Portal Plugin

- Plugin ID: `captive-portal`
- Direct Plugin dependencies: `webserver`
- Provided typed capabilities: `CaptivePortal`
- Required typed capabilities: `WebServer`
- Workflow Actions, Events, and Agent Tools: none

This plugin owns web entry aggregation and the portal scaffold resource entry.
It registers one GET subtree, `/portal/*`, with WebServer. It owns no additional
task, socket, or DNS service. OS captive-network detection paths and captive
DNS for the setup access point are owned by the `wifi` Plugin.

## Consumer contract

A plugin that contributes UI declares `captive-portal` as a dependency, obtains
`CaptivePortal`, and calls `register(WebEntry { id, group, order, title, summary,
icon, figure, module }, assets)`:

- `group` (`WebGroup::Device`, `Agent` or `Channel`) places the entry under
  设备 / 智能体 / 消息通道 in the navigation; `order` sorts it within the group
  (ascending, ties by ID).
- `title` and `summary` are `WebText { zh, en }`: the navigation label and the
  one line under it on overview tiles and rows.
- `icon` is an optional relative asset path: `.svg` is a monochrome mark the
  shell draws in `currentColor`; any other format is shown as an image.
- `figure` is an optional relative path of a live-figure module (see below).
- `module` is the relative entry module.
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
Module, icon, figure and resource paths are relative, at most 256 bytes, and use ASCII
letters, digits, `/`, `-`, `_`, and `.`. Empty, dot, and parent segments,
backslashes, percent escapes, and query characters are rejected. Generated
asset filenames should follow these constraints; URL queries are removed by
WebServer before dispatch.

## HTTP surface

- `/portal/entries.json` returns an array of
  `{id, group, order, title: {zh, en}, summary: {zh, en}, icon, figure, module}`
  in registration order. `group` is `"device"`, `"agent"` or `"channel"`.
  Asset URLs have the form `/portal/assets/<id>/<relative-path>`; an absent
  `icon` or `figure` is `null`. Labels are JSON escaped. The shell sorts by
  (group, order, id) and rejects the whole manifest if any record is malformed
  or names a URL outside its own `/portal/assets/<id>/`.
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

The shell is implemented in `resources/web/src/` using TypeScript, semantic
HTML, and CSS without a browser runtime framework or external fonts/CDNs. Its
stylesheet is the design system's `tokens.css` and `components/bundle.css`
(`src/ds/`, unchanged apart from formatting) plus the shell's own layout rules
(`src/app.css`). Bun emits `index.html`, `app.js`, and `app.css` into
`filesystem/resources/`. Build tools and test dependencies are managed at the
repository root, outside the packaged resource tree. A mounted volume without
the scaffold returns 404; an absent resource volume returns 503. System
currently mounts the durable `/data` volume; provisioning and mounting
`/resources` remains a System/image-integration requirement, not something this
plugin performs.

The shell is an app shell with a collapsible sidebar (Ctrl/⌘ B; the choice is
kept in `localStorage` as `barracuda.sidebar`), a breadcrumb top bar with the
language menu (简体中文 / English, `barracuda.lang`) and the theme control
(浅色 / 深色 / 跟随系统, `barracuda.theme`, applied as `data-theme` on `<html>`),
and hash routing: `#overview` (default) or `#<entry id>`; back and forward work.
Below 720px the sidebar is replaced by the phone layout: the overview becomes a
grouped list and pages get a back link.

The overview is generated from the manifest only: the header frame (title,
lead, the number of plugin pages, the connection, the `board` figure), one
「开始使用」 step per group that has entries (linking to its first entry), and the
module grid. Every device and agent entry, and every channel entry with a
`figure`, is a tile (figure, title, summary; the whole tile is a hover zone for
the figure). Channel entries without a figure are rows in the 消息通道 card
beside the `riffle` figure. The shell shows no device state it cannot read
(for example the Wi-Fi network name).

Status pages cover no entries (empty), an entry that is unknown or was removed
(unavailable; the shell names it if it saw it earlier), a module import in
progress (loading), and a module that failed to import or mount (failed, with
retry). The page imports a module only when selected. It refreshes the
manifest on request, on becoming visible, and every 30 seconds while visible.
A failed refresh preserves the last known entries and the mounted module and
shows 「连接未就绪」 in the top bar. Build-time exclusion and runtime route removal
are distinct: runtime unload does not erase image files.

### Live figures

The shell bundles the Hairline kernel once, publishes it as the global `HL`, and
defines `<hl-figure name="…">` (with `scan="true"` for an ambient sweep, hover
zones `data-hl-zone` and per-element points `data-hl-at`). Built-in names are
`board`, `riffle` and `plug`. Any other name is an entry ID: the shell imports
that entry's manifest `figure` module once (cached per URL) and mounts its
exported `figure` (`{ name, range, mount }`). Figures are cropped to the
design's box for their name, follow the theme through the `--figure-*` tokens,
hold still under reduced motion, sleep off screen, and are destroyed when their
element leaves the page. Contributor figure modules use the global `HL` and
never bundle the kernel.

## Bun build and module contract

From the repository root, `cargo plugin run build` builds resources for every
enabled contributor. `cargo plugin run build --plugin captive-portal` builds
only this shell. The same build task runs automatically through this Plugin's
Cargo build hook; ordinary `cargo build`, `cargo run`, and `cargo check` do not
require a separate resource-build command. This produces resource files, not a
System image or a mounted device volume. Bun must be the version CI pins
(`BUN_VERSION` in the workflow); older releases cannot read `bun.lock`.

The Plugin declares its commands in `plugin.toml`. `inputs` and `outputs` are
relative to the Plugin root, while `cwd` selects where the executable runs.
Only `build` is automatic. Frontend dependencies, formatting, lint, type checking,
and the test runner are shared across the repository. Bun must already be installed.

From the repository root:

```sh
bun install --frozen-lockfile
bun run format:check
bun run lint
bun run check
cargo plugin run build
bun run test
bun run dev
```

`dev` serves the built files on loopback and prints its URL (set `PORT` to
choose a port). Its manifest lists the nine built-in registrations; their
assets are served from each plugin's own `filesystem/resources/` when built and
are 404 otherwise, and no device API is served, so pages show what they show
when the device does not answer. `PORTAL_MANIFEST=empty` serves an empty
manifest. Device integration still uses the real Rust manifest. Rebuild and
refresh the preview after source changes.

Each contributor builds its entry independently as browser ESM into its own
`filesystem/resources/` with `tools/web/build.ts`: `resources/web/entry.ts`
becomes `entry.js`; an optional `resources/web/figure.js` (or `.ts`) becomes
`figure.js` (the build refuses one that bundles the kernel); an optional
`resources/web/icon.svg` or `icon.png` is copied. An optional output whose
source is gone is removed. The plugin registers those paths in its `WebEntry`
and lists them in its `[tasks.build] outputs`. Do not statically import plugin
modules into the scaffold. A module exports:

```ts
import type { PortalContext } from "../../../captive-portal/resources/web/ui";

export function mount(root: HTMLElement, context: PortalContext) {
  const label = document.createElement("p");
  label.textContent = context.lang === "zh" ? "模块内容" : "Module content";
  root.append(label);
  // Pass context.signal to fetch/listeners; clean up timers and other owned resources.
  return () => root.replaceChildren();
}
```

The context is `{ signal, lang, toast, navigate }`:

- `signal` is aborted on navigation, language change, unload, or page exit;
- `lang` (`"zh"` or `"en"`) is the language the module renders its own strings in;
  a language change remounts the module;
- `toast({ kind: "success" | "error" | "info", title, body?, code?, action? })`
  adds to the shell's toast stack (bottom-right; errors stay until closed,
  others close after 4 s);
- `navigate(id)` goes to another entry, or `"overview"`.

`toast` and `navigate` do nothing once the signal is aborted. `mount` may return
a promise of its cleanup function, or no cleanup function. The shell aborts the
signal and runs cleanup before removing the module's root. Every mount gets its
own DOM root; stale imports cannot mount into a new screen. Modules must handle
cancellation and avoid modifying the shell's global DOM. Modules are trusted
same-origin plugin code, not sandboxed third party content. Pages are built with
the shared UI kit in `resources/web/ui/` (documented in `resources/web/ui/README.md`):
it is bundled into each contributor at build time and never shared at runtime.
Load any module-specific CSS inside the contributor (and remove owned styles on
cleanup); the shell does not discover extra CSS files. Relative imports resolve
from the contributor's resource URL. Prefer content-hashed module filenames
across firmware versions; JavaScript module caching is browser-owned.

## Verification

Built-in contributors are `wifi`, `agent` (model configuration),
`agent-websearch` (Tavily), `imessage-qq`, `imessage-wechat`,
`imessage-bluebubble`, `imessage-telegram`, `imessage-inkbox`, and
`imessage-web` (live text chat). Each owns its `resources/web/entry.ts`, optional
figure and icon, tests, build declaration, and resource output. Ordinary entries
use `tools/web/build.ts`; this shell retains its custom build script. Chat-specific
styles live in the chat module and are removed on unmount. No dependency scan
creates navigation entries.

After changing a contributor or the build-time `resources/web/ui/` kit, rebuild
the affected contributor(s) with `cargo plugin run build` from the repository root.
The portal frontend tests verify all checked-in contributor outputs (entry,
figure and icon) against a fresh in-memory Bun build, manifest parsing, routing,
module session lifecycle and context, overview grouping, status pages, toasts,
live-figure loading and teardown, the kit's fields, validation, submission and
secret cleanup. These are DOM/protocol tests, not device or visual browser
verification.

Tests cover entry validation, duplicate IDs, namespace separation, resource
path traversal rejection, cached dispatch allocation counts, actual HTTP
manifest JSON escaping, scaffold file streaming, and Plugin Manager unload/reload
removing/restoring the manifest and asset access together.
