-- archive — preview archive contents, extract and compress.
--   X                 extract the hovered archive here
--   :compress <name>  compress marked items (.zip or .tar.gz/.tar.xz/...)

local M = {}

local windows = strata.platform == "windows"

local function q(s)
  if windows then
    return '"' .. s:gsub('"', '\\"') .. '"'
  end
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function basename(path)
  return path:match("([^/\\]+)$") or path
end

local function is_zip(path)
  local lower = path:lower()
  return lower:match("%.zip$") or lower:match("%.jar$")
end

local function list_cmd(path)
  if is_zip(path) and strata.which("unzip") then
    return "unzip -l " .. q(path)
  end
  if path:lower():match("%.7z$") and strata.which("7z") then
    return "7z l " .. q(path)
  end
  if path:lower():match("%.rar$") and strata.which("unrar") then
    return "unrar l " .. q(path)
  end
  return "tar -tvf " .. q(path)
end

function M.setup()
  strata.previewer({
    ext = { "zip", "jar", "tar", "gz", "tgz", "xz", "txz", "bz2", "zst", "7z", "rar" },
    fn = function(path, _, height)
      local out, code, err = strata.shell(list_cmd(path))
      if code ~= 0 or not out then
        return { "cannot list archive:", err or "" }
      end
      local lines = { "  " .. basename(path), "" }
      for line in out:gmatch("[^\n]+") do
        if #lines >= height then
          table.insert(lines, "…")
          break
        end
        table.insert(lines, line)
      end
      return lines
    end,
  })

  local function extract(ctx)
    local path = ctx.hovered
    if not path or ctx.scheme ~= "local" then
      strata.notify("extract: hover a local archive", "warn")
      return
    end
    local cmd
    if is_zip(path) and strata.which("unzip") then
      cmd = "unzip -o " .. q(path) .. " -d " .. q(ctx.cwd)
    else
      cmd = "tar -xf " .. q(path) .. " -C " .. q(ctx.cwd)
    end
    local _, code, err = strata.shell(cmd, ctx.cwd)
    if code == 0 then
      strata.notify("extracted " .. basename(path))
      strata.refresh()
    else
      strata.notify("extract failed: " .. (err or ""), "error")
    end
  end

  local function compress(ctx, name)
    local items = ctx.selected
    if #items == 0 and ctx.hovered then
      items = { ctx.hovered }
    end
    if #items == 0 or ctx.scheme ~= "local" then
      strata.notify("compress: nothing to compress", "warn")
      return
    end
    local function run(target)
      if not target or target == "" then
        return
      end
      local names = {}
      for _, p in ipairs(items) do
        table.insert(names, q(basename(p)))
      end
      local cmd
      if target:lower():match("%.zip$") then
        cmd = "zip -r " .. q(target) .. " " .. table.concat(names, " ")
      else
        cmd = "tar -caf " .. q(target) .. " " .. table.concat(names, " ")
      end
      local _, code, err = strata.shell(cmd, ctx.cwd)
      if code == 0 then
        strata.notify("created " .. target)
        strata.refresh()
      else
        strata.notify("compress failed: " .. (err or ""), "error")
      end
    end
    if name and name ~= "" then
      run(name)
    else
      strata.input("Archive name", basename(items[1]) .. ".tar.gz", run)
    end
  end

  strata.map("X", extract, "Extract archive here")
  strata.command("extract", extract, "Extract the hovered archive here")
  strata.command("compress", compress, "Compress marked items")
end

return M
