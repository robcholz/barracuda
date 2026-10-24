-- Sessions, a builtin app: the Agent's conversations, newest first. Opening
-- one shows its latest messages, the person's on the right and the Agent's
-- on the left, while the Agent is still answering too.
local app = require("app")
local sessions = require("sessions")
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

-- What an Agent message that only called Tools shows.
local function tools(names)
  return "Called " .. table.concat(names, ", ")
end

local function publish()
  set("clock", clock())
  set("page", page)
  if page == LIST then
    list = sessions.list() or {}
    local rows = {}
    for index, session in ipairs(list) do
      if index > 4 then break end
      rows[index] = { id = index, title = session.title, detail = session.when }
    end
    set("sessions", rows)
    set("empty", #rows == 0)
  elseif current then
    local rows, thinking = {}, false
    for index, message in ipairs(sessions.transcript(current.id) or {}) do
      local text = message.text
      if text == "" then text = tools(message.tools) end
      rows[index] = { id = index, mine = message.mine, text = text }
      thinking = thinking or message.open
    end
    set("title", current.title)
    set("messages", rows)
    set("thinking", thinking)
  end
end

set("sessions", {})
set("messages", {})
publish()
for event, value in app.events({ every = 5000 }) do
  if event == "open" then
    current = list[value]
    if current then
      page = CHAT
      app.navigation(1)
      publish()
    end
  elseif event == "app:back" then
    page = LIST
    current = nil
    app.navigation(0)
    publish()
  elseif event == "app:tick" or event == "app:foreground" then
    publish()
  elseif event == "app:terminate" then
    break
  end
end
