-- zoxide — jump to frequently used directories (https://github.com/ajeetdsouza/zoxide).
--   z   jump by query
--   Z   pick from the zoxide database
-- Directories you visit are added to zoxide automatically.

local M = {}

local function q(s)
  if strata.platform == "windows" then
    return '"' .. s .. '"'
  end
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

function M.setup()
  if not strata.which("zoxide") then
    return
  end

  strata.on("cd", function(ctx)
    if ctx.scheme == "local" then
      strata.shell("zoxide add " .. q(ctx.cwd))
    end
  end)

  strata.map("z", function()
    strata.input("zoxide", "", function(query)
      if not query or query == "" then
        return
      end
      local out, code = strata.shell("zoxide query -- " .. query)
      if code == 0 and out then
        strata.cd((out:gsub("%s+$", "")))
      else
        strata.notify("zoxide: no match for " .. query, "warn")
      end
    end)
  end, "Jump with zoxide")

  strata.map("Z", function()
    local out = strata.shell("zoxide query -l") or ""
    local dirs = {}
    for line in out:gmatch("[^\n]+") do
      table.insert(dirs, line)
      if #dirs >= 200 then
        break
      end
    end
    strata.select("zoxide", dirs, function(dir)
      if dir then
        strata.cd(dir)
      end
    end)
  end, "Pick a frequent directory")
end

return M
