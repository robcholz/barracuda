# App API

The app API is what an installable app can count on from one device to the
next: the Lua packages it may use, what it may ask the device, and the rules
that keep both stable. It is versioned by one number, the **API level**; this
system implements level 1. An app names the level it is written for in its
manifest's `api` (see [`app-format.md`](app-format.md)), and a device whose
level is lower refuses to install it.

System apps are not bound by the app API: they ship in the image and may
`require` every package.

## Packages

Every Lua package places itself in one of four tiers. A Plugin declares its
package's tier with `LuaPackage::app_api`, and the capability's purpose there
is the line the install screen shows; `crates/appkit/src/device.rs` holds the
level-1 list the install check uses, so a device can name a capability no
Plugin on it provides.

| Tier | How an app gets it | Same on every device |
| --- | --- | --- |
| Core | always, without asking | yes |
| Capability | `requires` or `uses` in the manifest | yes, where the device has it |
| Device-specific | `permissions`, with `boards` | no: it talks to one Board's hardware |
| Other | `permissions` | no promise |

### Core

| Package | Since | What |
| --- | --- | --- |
| `app` | 1 | the app itself: events, view, storage, `app.device()` ([`app-format.md`](app-format.md#api)) |
| `time` | 1 | UTC time through the standard `os.time`, `os.date`, `os.difftime` ([`vm-time`](../../vm-time/docs/plugin.md)) |

### Capabilities

| Package | Since | Install screen | Lua API |
| --- | --- | --- | --- |
| `agent` | 1 | 使用 Agent | `agent.ask(text, stream)` ([`vm-agent`](../../vm-agent/docs/plugin.md)) |
| `audio` | 1 | 播放声音和录音 | `audio.available()`, `audio.open()` ([`audio`](../../vm-audio/docs/plugin.md)) |
| `camera` | 1 | 使用相机拍照 | `camera.available()`, `camera.open()` ([`camera`](../../vm-camera/docs/plugin.md)) |
| `filesystem` | 1 | 读写共享文件 | the standard `io` and `os` file functions over the shared Workspace ([`vm-filesystem`](../../vm-filesystem/docs/plugin.md)) |
| `http` | 1 | 联网收发数据 | `http.request(...)` ([`vm-http`](../../vm-http/docs/plugin.md)) |
| `light` | 1 | 控制灯光 | `light.available()`, `light.open()` ([`led-strip`](../../vm-led-strip/docs/plugin.md)) |
| `motion` | 1 | 读取动作传感器 | `motion.available()`, `motion.open()` ([`imu`](../../vm-imu/docs/plugin.md)) |
| `notify` | 1 | 发通知、向你确认 | `notify.post(title, body)`, `notify.ask(title, body)` (below) |

A device has a capability when its Plugin is enabled and, for the hardware
ones (`audio`, `camera`, `light`, `motion`), the Board has the device: on a
Board without an LED strip, `light` is not a capability, and an app that
requires it is not installed.

#### `notify`

The `app-manager` Plugin provides it.

- `notify.post(title [, body]) -> id` adds a notice to the notification
  center.
- `notify.ask(title [, body]) -> allowed` asks the person on a card listed
  before every notice and waits for the answer: `true` when they allow it,
  `false` when they deny or dismiss it.

Failures return `nil` and a message.

### Device-specific

`analog`, `display`, `gpio`, `i2c`, `i2s`, `power_monitor`, `pwm`, `spi`,
`uart`. Each reaches pins, buses, or a peripheral directly, so what it does
depends on the Board's wiring. An app that lists one in `permissions` must
also list the Boards it was written for in `boards`; a manifest without
`boards` is not valid, and a device whose Board is not listed refuses it.

### Other

`webserver`, `message_queue`, `systeminfo`, and any package a later Plugin
adds without a tier. An app may ask for them in `permissions`, but their
interface may change with any system update.

## Install check

The system checks an offered app against the device before installing it,
in this order, and stops at the first reason. `app.list` reports the reason
as the app's `incompatible`; the install screen shows it and disables the
install button; `app.install` answers `{"error": "incompatible", "reason": …}`.

| Check | Reason |
| --- | --- |
| `api` is above the device's level | 需要更新系统（应用需要 API 2，本机是 1） |
| `boards` does not list this Board | 只适用于 espressif-esp-vocat-v1-2 |
| the app has a view and the device no display | 此设备没有屏幕 |
| `view.size` or `view.shape` differs from the display | 屏幕不匹配（应用为 240×240 方屏） |
| a capability in `requires` is missing | 此设备没有灯光 |
| a package in `permissions` is missing | 此设备不支持 serial |

An installed app that no longer fits, after a Board or system change, does
not open; `app.open` answers `incompatible`.

## Runs

A run of an installable app may `require` the core packages and every
package its manifest names in `requires`, `uses`, and `permissions`. A
package the app did not name is not there, and `require` fails. A capability
in `uses` may be missing: check `app.device().capabilities` before requiring
it, or call `pcall(require, name)`.

`app.device()` describes the device:

```lua
{
  api = 1,
  board = "espressif-esp-vocat-v1-2",
  display = { width = 360, height = 360, round = true },  -- nil without one
  touch = true,
  capabilities = { http = true, light = true, notify = true, ... },
}
```

## Versioning

- A capability's interface never changes once it is in a level: no function
  is removed, renamed, or given other arguments or results.
- Adding functions to a capability, or adding a capability, raises the API
  level; the new parts are documented with the level they arrived in.
- An incompatible change is a new capability with a new name; the old one
  stays.
- A device-specific or other package may change with any system update;
  that is why apps that use them name their Boards.
