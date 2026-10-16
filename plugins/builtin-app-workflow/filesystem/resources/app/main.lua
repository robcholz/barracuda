-- Workflows, a builtin app: the loaded Workflows and what starts each.
local app = require("app")
local workflows = require("workflows")
local view = assert(app.view())

local GLYPHS = { ["定时"] = "\u{E090}", ["网络变化时"] = "\u{E09C}" }
local OTHER = "\u{E004}"

local function clock()
  local ok, now = pcall(os.date, "%H:%M")
  return ok and now or ""
end

local function publish()
  assert(view:set("clock", clock()))
  local rows = {}
  for index, flow in ipairs(workflows.list() or {}) do
    if index > 4 then break end
    rows[index] = {
      id = index,
      name = flow.id,
      detail = flow.trigger,
      glyph = GLYPHS[flow.trigger] or OTHER,
      on = true,
    }
  end
  assert(view:set("flows", rows))
  assert(view:set("empty", #rows == 0))
end

assert(view:set("flows", {}))
publish()
for event in app.events({ every = 15000 }) do
  if event == "app:tick" or event == "app:foreground" then
    publish()
  elseif event == "app:terminate" then
    break
  end
end
