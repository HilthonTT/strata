-- archive — list the contents of archives in the preview, including formats
-- strata can't open itself (7z, rar) when 7z or unrar is installed.
-- Extracting (X, :extract) and compressing (:compress) are built in.

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
end

return M
