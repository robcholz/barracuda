-- Lamp: an example installable app that switches the Board's LED strip and
-- picks warm or cool light. It requires the `light` capability.
local app = require("app")
local light = require("light")
local view = assert(app.view())

local LEVEL = 60 -- percent
local COLORS = { warm = { 255, 147, 41 }, cool = { 201, 226, 255 } }
local saved = app.kv.get("lamp") or {}
local on, tone = saved.on ~= false, saved.tone or "warm"
local strip = light.available() and light.open() or nil

local function shine()
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
  shine()
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
