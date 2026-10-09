-- bookmarks — named directory bookmarks that persist between sessions.
--   B   bookmark the current directory
--   '   jump to a bookmark
--   :bookmark-remove   delete one

local M = {}

local function store()
  return strata.data_dir() .. "/bookmarks.tsv"
end

local function load()
  local marks = {}
  local f = io.open(store(), "r")
  if f then
    for line in f:lines() do
      local name, path = line:match("^(.-)\t(.+)$")
      if name then
        table.insert(marks, { name = name, path = path })
      end
    end
    f:close()
  end
  return marks
end

local function save(marks)
  strata.mkdir(strata.data_dir())
  local f, err = io.open(store(), "w")
  if not f then
    strata.notify("bookmarks: " .. tostring(err), "error")
    return
  end
  for _, m in ipairs(marks) do
    f:write(m.name, "\t", m.path, "\n")
  end
  f:close()
end

local function labels(marks)
  local items = {}
  for _, m in ipairs(marks) do
    table.insert(items, m.name .. "  →  " .. m.path)
  end
  return items
end

local function find(marks, item)
  for i, m in ipairs(marks) do
    if item == m.name .. "  →  " .. m.path then
      return i, m
    end
  end
end

local function add(ctx)
  local default = ctx.cwd:match("([^/\\]+)[/\\]?$") or ctx.cwd
  strata.input("Bookmark name", default, function(name)
    if not name or name == "" then
      return
    end
    local marks = load()
    for i = #marks, 1, -1 do
      if marks[i].name == name then
        table.remove(marks, i)
      end
    end
    table.insert(marks, { name = name, path = ctx.cwd })
    save(marks)
    strata.notify("bookmarked " .. name)
  end)
end

local function jump()
  local marks = load()
  if #marks == 0 then
    strata.notify("no bookmarks yet — press B to add one", "warn")
    return
  end
  strata.select("Bookmarks", labels(marks), function(item)
    local _, m = find(marks, item)
    if m then
      strata.cd(m.path)
    end
  end)
end

function M.setup()
  strata.map("B", add, "Bookmark this directory")
  strata.map("'", jump, "Jump to a bookmark")
  strata.command("bookmark", add, "Bookmark this directory")
  strata.command("bookmarks", jump, "Jump to a bookmark")
  strata.command("bookmark-remove", function()
    local marks = load()
    strata.select("Remove bookmark", labels(marks), function(item)
      local i = find(marks, item)
      if i then
        table.remove(marks, i)
        save(marks)
        strata.notify("bookmark removed")
      end
    end)
  end, "Remove a bookmark")
end

return M
