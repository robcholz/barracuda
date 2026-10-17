-- Settings, a builtin app: Wi-Fi, sound and display, apps, storage, and the
-- device, through its own `settings` package.
local app = require("app")
local settings = require("settings")
local view = assert(app.view())

local SETTINGS, WIFI, SOUND, APPS, STORAGE, ABOUT = 0, 1, 2, 3, 4, 5
-- The width of a level bar, in pixels: the display less the page's 40 px
-- margins and its rows' 12 px padding.
local DISPLAY = app.device().display
local BAR = (DISPLAY and DISPLAY.width or 360) - 2 * 40 - 2 * 12
local TIMEOUTS = { 15, 30, 60, 300, 0 }
local STATES = {
  running = "运行中",
  background = "后台",
  suspended = "已挂起",
  terminating = "正在退出",
}
local EMPTY = { base = "", accent = "", dim = "" }

local page = SETTINGS
local status = assert(settings.status())
local networks = {}

local function clock()
  local ok, now = pcall(os.date, "%H:%M")
  return ok and now or ""
end

local function set(name, value)
  assert(view:set(name, value))
end

local function timeout_text(seconds)
  if seconds == 0 then return "永不" end
  if seconds < 60 then return seconds .. " 秒" end
  return (seconds // 60) .. " 分钟"
end

local function installed()
  local list = {}
  for _, info in ipairs(assert(settings.apps())) do
    list[#list + 1] = info
  end
  return list
end

local function settings_rows()
  local wifi = status.wifi
  local storage = assert(settings.storage())
  return {
    {
      id = WIFI,
      label = "Wi-Fi",
      value = wifi.ssid or (wifi.state == "connected" and "已连接" or "未连接"),
      glyph = "\u{E09C}",
      mono = wifi.ssid ~= nil,
    },
    { id = SOUND, label = "声音与显示", value = "", glyph = "\u{E090}", mono = false },
    { id = APPS, label = "应用", value = string.format("%02d", #installed()), glyph = "\u{E060}", mono = true },
    { id = STORAGE, label = "存储", value = storage.label .. " 已用", glyph = "\u{E034}", mono = false },
    { id = ABOUT, label = "关于本机", value = "", glyph = "\u{E040}", mono = false },
  }
end

local function publish_wifi()
  local wifi = status.wifi
  set("connected", wifi.state == "connected")
  set("named", wifi.ssid ~= nil)
  set("ssid", wifi.ssid or (wifi.state == "connected" and "已连接的网络" or "未连接"))
  set("network", wifi.state == "connected" and "已连接" or (wifi.state == "connecting" and "连接中" or ""))
  local shown = {}
  for index, item in ipairs(networks) do
    if item.ssid ~= wifi.ssid and #shown < 4 then
      shown[#shown + 1] = { id = index, ssid = item.ssid, signal = item.glyphs, locked = item.secured }
    end
  end
  set("networks", shown)
end

local function publish_sound()
  set("volume", status.volume)
  set("volumeFill", status.volume * BAR // 100)
  set("brightness", status.brightness)
  set("brightnessFill", status.brightness * BAR // 100)
  set("timeout", timeout_text(status.screen_off))
  set("muted", status.muted)
end

local function publish_apps()
  local rows = {}
  for index, info in ipairs(installed()) do
    local icon = info.glyphs
    local plain = icon.base == ""
    rows[index] = {
      id = index,
      name = info.name,
      icon = icon,
      letter = plain and info.letter or "",
      state = STATES[info.state] or ("版本 " .. info.version),
      live = info.state == "running",
    }
  end
  set("apps", rows)
end

local function publish_storage()
  local storage = assert(settings.storage())
  set("used", storage.label .. " 已用")
  if storage.total then
    set("usedNote", storage.free_label .. " 可用，共 " .. storage.total_label)
  else
    set("usedNote", "媒体、应用与缓存")
  end
  local total = math.max(storage.total or storage.used, storage.used, 1)
  local fills = {}
  local rows = {}
  local glyphs = { image = "\u{E03C}", package = "\u{E060}", memory = "\u{E050}" }
  for index, part in ipairs(storage.parts) do
    fills[index] = part.bytes * BAR // total
    rows[index] = { id = index, label = part.name, value = part.label, glyph = glyphs[part.icon] or "", mono = true }
  end
  set("mediaFill", fills[1])
  set("appsFill", fills[2])
  set("cacheFill", fills[3])
  set("rows", rows)
end

local function publish_about()
  local device = status.device
  local storage = assert(settings.storage())
  set("board", device.board)
  set("rows", {
    { id = 1, label = "系统版本", value = device.version, glyph = "\u{E040}", mono = true },
    { id = 2, label = "芯片", value = device.chip, glyph = "\u{E050}", mono = true },
    { id = 3, label = "存储", value = storage.free_label and (storage.free_label .. " 可用") or (storage.label .. " 已用"), glyph = "\u{E034}", mono = false },
  })
end

local function publish()
  set("clock", clock())
  set("page", page)
  if page == SETTINGS then
    set("rows", settings_rows())
  elseif page == WIFI then
    publish_wifi()
  elseif page == SOUND then
    publish_sound()
  elseif page == APPS then
    publish_apps()
  elseif page == STORAGE then
    publish_storage()
  elseif page == ABOUT then
    publish_about()
  end
end

local function go(to)
  page = to
  app.navigation(page == SETTINGS and 0 or 1)
  if page == WIFI then
    set("scanning", true)
    publish()
    networks = settings.scan() or {}
    set("scanning", false)
  end
  publish()
end

local function change(key, value)
  status = assert(settings.set(key, value))
  publish()
end

set("networks", {})
set("apps", {})
publish()
for event, value in app.events({ every = 15000 }) do
  if event == "open" then
    go(value)
  elseif event == "app:back" then
    go(SETTINGS)
  elseif event == "setVolume" then
    change("volume", value)
  elseif event == "setBrightness" then
    change("brightness", value)
  elseif event == "nextTimeout" then
    local next = TIMEOUTS[1]
    for index, seconds in ipairs(TIMEOUTS) do
      if seconds == status.screen_off then
        next = TIMEOUTS[index % #TIMEOUTS + 1]
      end
    end
    change("screen_off", next)
  elseif event == "toggleMute" then
    change("muted", not status.muted)
  elseif event == "app:tick" or event == "app:foreground" then
    status = assert(settings.status())
    publish()
  elseif event == "app:terminate" then
    break
  end
end
