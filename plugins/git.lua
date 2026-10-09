-- git — branch and change count in the status line, and a "Git" panel
-- listing `git status`. Toggle the panel with `g s`.
--
-- Options ([plugins.options.git]):
--   panel = true   -- show the panel at startup

local M = {}

local cache = { cwd = nil, at = 0, branch = nil, lines = {} }

local function refresh(cwd)
  if cache.cwd == cwd and os.time() - cache.at < 3 then
    return
  end
  cache.cwd, cache.at, cache.branch, cache.lines = cwd, os.time(), nil, {}
  local branch, code = strata.shell("git rev-parse --abbrev-ref HEAD", cwd)
  if code ~= 0 or not branch then
    return
  end
  cache.branch = branch:gsub("%s+$", "")
  local status = strata.shell("git status --short", cwd) or ""
  for line in status:gmatch("[^\n]+") do
    table.insert(cache.lines, line)
  end
end

function M.setup(opts)
  strata.statusline(function(ctx)
    if ctx.scheme ~= "local" then
      return nil
    end
    refresh(ctx.cwd)
    if not cache.branch then
      return nil
    end
    local icon = ctx.icons and "\u{e0a0} " or "⎇ "
    if #cache.lines > 0 then
      return icon .. cache.branch .. " ±" .. #cache.lines
    end
    return icon .. cache.branch
  end)

  strata.panel({
    name = "git",
    title = "Git",
    render = function(ctx, width, height)
      if ctx.scheme ~= "local" then
        return { "not a local directory" }
      end
      refresh(ctx.cwd)
      if not cache.branch then
        return { "not a git repository" }
      end
      local out = { "branch: " .. cache.branch, "" }
      if #cache.lines == 0 then
        table.insert(out, "working tree clean")
      end
      for i, line in ipairs(cache.lines) do
        if #out >= height then
          table.insert(out, "… " .. (#cache.lines - i + 1) .. " more")
          break
        end
        table.insert(out, line)
      end
      return out
    end,
  })

  strata.map("g s", function()
    strata.toggle_panel("git")
  end, "Toggle the git panel")

  strata.command("git", function(_, args)
    strata.exec("git " .. args)
  end, "Run a git command in this directory")

  if opts.panel then
    strata.toggle_panel("git")
  end
end

return M
