-- Sessions, a system app: the Agent's conversations, newest first. Opening
-- one shows what was first asked in it and the Agent's latest words here.
local app = require("app")
local system = require("system")
local view = assert(app.view())

local LIST, CHAT = 0, 1
local page = LIST
local list = {}
local current = nil

local function clock()
  local ok, now = pcall(os.date, "%H:%M")
  return ok and now or ""
end

local function set(name, value)
  assert(view:set(name, value))
end

local function publish()
  set("clock", clock())
  set("page", page)
  if page == LIST then
    list = system.sessions() or {}
    local rows = {}
    for index, session in ipairs(list) do
      if index > 4 then break end
      rows[index] = { id = index, title = session.title, detail = session.when }
    end
    set("sessions", rows)
    set("empty", #rows == 0)
  elseif current then
    local status = system.status() or {}
    set("asked", current.title)
    set("when", current.when ~= "" and current.when or "Agent")
    local agent = status.agent or {}
    set("said", agent.answer or "")
    set("thinking", agent.state == "thinking")
  end
end

set("sessions", {})
publish()
for event, value in app.events({ every = 15000 }) do
  if event == "open" then
    current = list[value]
    if current then
      page = CHAT
      app.navigation(1)
      publish()
    end
  elseif event == "app:back" then
    page = LIST
    app.navigation(0)
    publish()
  elseif event == "app:tick" or event == "app:foreground" then
    publish()
  elseif event == "app:terminate" then
    break
  end
end
