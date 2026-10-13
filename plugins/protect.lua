-- protect — refuse to delete, move or overwrite items tagged "keep".
--   Folders holding a protected item are protected too.
--   Shows a lock column next to protected items.
--   Options: tag = "keep", column = true

local M = {}

-- The first protected path among `paths`, using the tags the event carries
-- (they cover the paths and everything tagged inside them).
local function protected(e, tag)
  for path, tags in pairs(e.tags) do
    for _, t in ipairs(tags) do
      if t == tag then
        return path
      end
    end
  end
end

local function name_of(path)
  return path:match("([^/\\]+)[/\\]?$") or path
end

function M.setup(opts)
  local tag = opts.tag or "keep"

  local function guard(verb)
    return function(_, e)
      local path = protected(e, tag)
      if path then
        return false, "protect: '" .. name_of(path) .. "' is tagged " .. tag .. "; not " .. verb
      end
    end
  end

  strata.on("before_delete", guard("deleting"))
  strata.on("before_move", guard("moving"))
  strata.on("before_rename", guard("renaming"))
  strata.on("before_overwrite", guard("overwriting"))

  if opts.column ~= false then
    strata.column({
      name = "protect",
      width = 2,
      style = "warning",
      fn = function(entry)
        for _, t in ipairs(entry.tags) do
          if t == tag then
            return "🔒"
          end
        end
      end,
    })
  end
end

return M
