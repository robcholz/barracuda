-- Settings, a system app: it requires `systeminfo` without asking, which an
-- installable app could only do with the permission.
local app = require("app")
local info = require("systeminfo")
local view = assert(app.view())

local sections = {
  {
    title = "本机",
    rows = {
      { "板子", info.board.name },
      { "芯片", info.board.chip },
    },
  },
  {
    title = "系统",
    rows = {
      { "平台", info.platform.name },
      { "架构", info.platform.architecture },
      { "版本", app.version },
    },
  },
}
local current = 1

local function publish()
  local section = sections[current]
  local rows = {}
  for index, row in ipairs(section.rows) do
    rows[index] = { id = index, label = row[1], value = tostring(row[2]) }
  end
  assert(view:set("section", section.title))
  assert(view:set("rows", rows))
  assert(view:set("page", current))
  assert(view:set("pages", #sections))
end

publish()
for event in app.events() do
  if event == "next" then
    current = current % #sections + 1
    publish()
  elseif event == "app:terminate" then
    break
  end
end
