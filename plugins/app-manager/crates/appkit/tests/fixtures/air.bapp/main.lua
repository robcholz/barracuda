-- Air quality: an example installable app that shows indoor PM2.5 and the
-- reading an hour ago. Boards have no particulate sensor yet, so `sample`
-- stands in for one; the app asks for no packages.
local app = require("app")
local view = assert(app.view())

local MINUTE = 60000
local history = app.kv.get("history") or {}

local function sample(step)
  return 10 + (step * 7) % 9
end

local function publish()
  local now = history[#history]
  local hour = history[math.max(#history - 60, 1)]
  assert(view:set("reading", tostring(now)))
  local good = now <= 35
  assert(view:set("good", good))
  assert(view:set("verdict", good and "空气良好" or (now <= 75 and "轻度污染" or "污染较重")))
  assert(view:set("before", "一小时前 " .. hour))
end

local step = #history
local function measure()
  step = step + 1
  history[#history + 1] = sample(step)
  if #history > 61 then table.remove(history, 1) end
end

measure()
publish()
for event in app.events({ every = MINUTE }) do
  if event == "app:tick" then
    measure()
    publish()
  elseif event == "app:background" then
    assert(app.kv.set("history", history))
  elseif event == "app:terminate" then
    assert(app.kv.set("history", history))
    break
  end
end
