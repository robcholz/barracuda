# Files Plugin

- Plugin ID: `files`
- Direct Plugin dependencies: `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `WebServer`, `CaptivePortal`
- Workflow Actions, Events and Agent Tools: none
- Owned long-running tasks: none

The Files Plugin is the portal's file browser. It registers the 文件/Files
entry in the Device group, after Wi-Fi, with the cabinet figure, and the
`/api/files` routes the page uses. It declares `PluginFilesystem::Inspect`:
its view holds the Workspace and, read-only beneath `/plugins`, every Plugin's
private trees (`/plugins/{data,cache,media,resources}/<id>`). It keeps no
state of its own: no KV records and nothing under its own `/data`.

## Routes

Paths are the view's real paths, beneath `/workspace` or `/plugins` only. In
a URL they are percent-encoded after the route prefix; in a JSON body they are
plain strings. Every segment is decoded and checked: an empty name, `.`, `..`,
a backslash, a control character or invalid UTF-8 is refused with 400, as is
any other root, so the Plugin's own namespace and the System trees stay out of
reach.

| Route | Answer |
| --- | --- |
| `GET /api/files/list/<path>` | `{"entries":[{"name","type":"dir"\|"file","size"}],"truncated"}`, directories first, at most 256 |
| `GET /api/files/raw/<path>` | the file's bytes, streamed |
| `PUT /api/files/upload/<path>` | `201` and the new entry; the body streams to the file |
| `POST /api/files/mkdir` `{"path"}` | `201`; the parent must exist |
| `POST /api/files/rename` `{"from","to"}` | `204`; both in the same directory |
| `POST /api/files/delete` `{"path"}` | `204`; a file or an empty directory |

An upload, a new directory or a rename onto an existing name answers 409; the
browser never overwrites. An upload streams through `WebServer::serve_upload`
into `.<name>.part` beside the target and is renamed into place once its
declared length has arrived; a body that ends early answers 400 and the
partial file is removed.

Which trees may change is decided by the view, not by the routes: a change
beneath `/plugins` or `/workspace/resources` fails in the file layer with
`ReadOnly` and answers 403 `{"error":"read_only"}`. Other refusals are
`{"error":<code>}` with 404 for a missing path, 409 for an existing name, a
non-empty directory or the wrong type, and 503 for a card removed mid-request.

`raw` serves under the portal's origin, so it names only types a browser will
not run: images and audio by their type, text as `text/plain`, everything else,
HTML and SVG included, as `application/octet-stream`.

## Portal

The page groups the Plugin trees by Plugin, `/plugins/<tree>/<id>` shown as
插件/`<id>`/`<tree>`, and shows them without any change controls. Modified
times are not shown: VFS metadata carries none. A storage card appears only
while one is mounted under `/workspace/removable`. The tree and pane rules
come with the page, added while it is open: the shell's stylesheet is held to
its flash budget and does not carry them.

## Retained registrations

The Plugin retains its portal entry and six route registrations for its
lifetime; unloading it removes the page and every route.
