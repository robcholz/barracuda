-- Workflows, a builtin app: the loaded Workflows, what starts each, and a
-- switch that turns each on or off.
local app = require("app")
local workflows = require("workflows")
local view = assert(app.view())

local GLYPHS = { ["定时"] = "\u{E090}", ["网络变化时"] = "\u{E09C}" }
local OTHER = "\u{E004}"

local function clock()
  local ok, now = pcall(os.date, "%H:%M")
  return ok and now or ""
end

local list = {}

local function publish()
  assert(view:set("clock", clock()))
  list = workflows.list() or {}
  local rows = {}
  for index, flow in ipairs(list) do
    if index > 4 then break end
    rows[index] = {
      id = index,
      name = flow.id,
      detail = flow.trigger,
      glyph = GLYPHS[flow.trigger] or OTHER,
      on = flow.enabled,
    }
  end
  assert(view:set("flows", rows))
  assert(view:set("empty", #rows == 0))
end

assert(view:set("flows", {}))
publish()
for event, value in app.events({ every = 15000 }) do
  if event == "toggle" then
    local flow = list[value]
    if flow then
      workflows.set_enabled(flow.id, not flow.enabled)
      publish()
    end
  elseif event == "app:tick" or event == "app:foreground" then
    publish()
  elseif event == "app:terminate" then
    break
  end
end
