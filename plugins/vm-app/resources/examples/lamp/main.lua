-- Lamp: an example installable app that switches the Board's LED strip and
-- picks warm or cool light. It asks for the `led_strip` package.
local app = require("app")
local led_strip = require("led_strip")
local view = assert(app.view())

local LEVEL = 60 -- percent
local COLORS = { warm = { 255, 147, 41 }, cool = { 201, 226, 255 } }
local saved = app.kv.get("lamp") or {}
local on, tone = saved.on ~= false, saved.tone or "warm"
local strip = led_strip.available() and led_strip.open() or nil

local function light()
  if strip == nil then return end
  if not on then
    strip:clear()
    return
  end
  local color = COLORS[tone]
  local pixel = string.char(
    color[1] * LEVEL // 100,
    color[2] * LEVEL // 100,
    color[3] * LEVEL // 100
  )
  strip:write(string.rep(pixel, strip:len()))
end

local function publish()
  assert(view:set("on", on))
  assert(view:set("warm", tone == "warm"))
  local name = tone == "warm" and "暖光" or "冷光"
  assert(view:set("status", on and (name .. " · " .. LEVEL .. "%") or "已关闭"))
  light()
end

publish()
for event in app.events() do
  if event == "power" then
    on = not on
    publish()
  elseif event == "setWarm" then
    tone = "warm"
    publish()
  elseif event == "setCool" then
    tone = "cool"
    publish()
  elseif event == "app:background" or event == "app:terminate" then
    assert(app.kv.set("lamp", { on = on, tone = tone }))
    if event == "app:terminate" then break end
  end
end
