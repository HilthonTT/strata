# Writing plugins

A plugin is a Lua 5.4 file. strata loads every `*.lua` file and every `*/init.lua` file in `~/.config/strata/plugins/` at startup. To skip a plugin, list its name under `plugins.disabled`.

```lua
-- ~/.config/strata/plugins/hello.lua
strata.map("g p", function(ctx)
  strata.cd(os.getenv("HOME") .. "/projects")
end, "Go to projects")

strata.command("hello", function(ctx, args)
  strata.notify("hello " .. args .. " from " .. ctx.cwd)
end, "Say hello")
```

If the file returns a table with a `setup` function, strata calls it with the plugin's options from `config.toml`:

```lua
local M = {}
function M.setup(opts)          -- [plugins.options.hello] greeting = "hi"
  strata.statusline(function() return opts.greeting end)
end
return M
```

## Context

Callbacks receive a `ctx` table that describes the current UI state:

| Field | Description |
| --- | --- |
| `cwd` | directory of the focused panel |
| `hovered` | path under the cursor, or `nil` |
| `hovered_is_dir` | whether `hovered` is a directory |
| `selected` | list of marked paths |
| `panel` | focused panel number (1-based) |
| `view` | `files`, `dashboard`, `docker`, `nas` or `usage` |
| `theme` | active theme name |
| `scheme` | `local`, `sftp` or `docker` |
| `icons` | whether Nerd Font icons are enabled |
| `tags` | tags of the hovered item |
| `filter` | the panel's filter text |
| `listing` | what a flat listing shows (`find *.log`, `flatten`…), or `nil` |
| `tab` | tab number (1-based) |

## API

**Registration**

| Function | Description |
| --- | --- |
| `strata.map(keys, fn(ctx), desc?)` | Bind a key sequence such as `"g p"` or `"ctrl+e"` |
| `strata.command(name, fn(ctx, args), desc?)` | Add a `:name` command |
| `strata.on(event, fn(ctx, data))` | React to an event (see [Events](#events)) |
| `strata.statusline(fn(ctx) -> string?)` | Add a segment to the top bar |
| `strata.panel{ name, title?, render = fn(ctx, w, h) -> lines }` | Add a UI panel next to the file panels |
| `strata.previewer{ ext = {...}, fn = fn(path, w, h) -> lines? }` | Preview files with these extensions; return `nil` to pass |
| `strata.column{ name, width?, style?, fn = fn(entry) -> text?, style? }` | Add a column to the file list (see [Columns and sort orders](#columns-and-sort-orders)) |
| `strata.sort{ name, title?, fn = fn(entry) -> key }` | Add a sort order to the sort menu (`s`) and `:sort <name>` |

**Actions**

| Function | Description |
| --- | --- |
| `strata.cd(path)` | Change directory |
| `strata.action(name)` | Run a built-in action (see `?` in strata), e.g. `"toggle_hidden"` |
| `strata.run(line)` | Run a command-palette line, e.g. `"theme nord"` |
| `strata.notify(msg, level?)` | Show a message (`"info"`, `"warn"` or `"error"`) |
| `strata.select(title, items, fn(item?))` | Show a fuzzy picker |
| `strata.input(prompt, default?, fn(text?))` | Show a text prompt |
| `strata.toggle_panel(name)` | Show or hide a plugin panel |
| `strata.exec(cmd)` | Run an interactive command with the terminal handed over |
| `strata.refresh()` | Reload the directory listings |
| `strata.mark(paths, clear?)` | Mark these paths in the focused panel; `clear` drops other marks first |
| `strata.show(title, paths)` | Show these paths as a flat list in the focused panel |
| `strata.tag(paths, tag)` / `strata.untag(paths, tag?)` | Add or remove a tag (all tags without one); `paths` may be one path |

## Events

Handlers get the context and a `data` table describing what happened:

| Event | `data` fields |
| --- | --- |
| `startup`, `quit`, `cd`, `paste` | none |
| `hover` | `path`: the item the cursor moved to |
| `open` | `path`: a file being opened |
| `create` | `path`, `dir` (true for a directory) |
| `rename` | `from`, `to`; once per item, also for pattern and bulk renames |
| `delete` | `paths`, `permanent` (false when going to the trash) |
| `tag` | `paths`, `tag` (`""` when every tag was removed), `added` |
| `find` | `title` (`find …`, `flatten`, `tag red`…), `count`, `truncated` |
| `job_done` | `label`, `state` (`done`, `failed` or `cancelled`), `error` |
| `mount` | `device`, `path`: a removable drive was mounted |
| `unmount` | `device`: a removable drive was ejected |
| `view` | `view`: `files`, `dashboard`, `docker`, `nas` or `usage` |
| `tab` | `tab`: the tab switched to (1-based) |
| `theme` | `name`: the theme picked |
| `layout_save`, `layout_load` | `name`: the layout saved or opened (`:layout`, `--layout`, `default_layout`) |

```lua
strata.on("rename", function(ctx, e)
  strata.notify(e.from .. " → " .. e.to)
end)
```

### Refusing operations

`before_*` events fire before an operation starts, and a handler can cancel it by returning `false` (optionally followed by a reason) or `{ cancel = true, reason = "..." }`. The reason is shown to the user; anything else lets the operation go ahead.

| Event | `data` fields |
| --- | --- |
| `before_delete` | `paths`, `permanent` |
| `before_rename` | `paths`, `to` (the new paths, in the same order); once per rename, pattern rename or bulk rename |
| `before_copy`, `before_move` | `paths`, `dest` (the destination folder) |
| `before_overwrite` | `paths` (the items that would be replaced), `sources`, `dest`; when a paste overwrites |

Every `before_*` event also carries `tags`: a table from path to tags for each item in `paths`, and for every tagged item inside them. The [`protect`](../plugins/protect.lua) plugin uses it to refuse deleting, moving, renaming or overwriting anything tagged `keep`:

```lua
strata.on("before_delete", function(ctx, e)
  for path, tags in pairs(e.tags) do
    for _, t in ipairs(tags) do
      if t == "keep" then return false, path .. " is tagged keep" end
    end
  end
end)
```

A handler that fails, or runs for more than half a second, is reported and does not cancel anything. Undo and redo do not ask.

### Columns and sort orders

Column and sort functions get an `entry` table: `path`, `name`, `is_dir`, `size`, `modified` (seconds since the epoch, or `nil`), `ext` (lower case, without the dot) and `tags`.

A column function returns the cell's text and optionally a colour: a theme role (`muted`, `accent`, `error`, `warning`, `success`, `info`, `fg`), a tag colour (`red`, `blue`…), a colour name or `#rrggbb`. Columns go after the built-in ones and are hidden first when a panel is narrow. Cells are worked out only for rows that come into view and are kept until the listing changes, so large folders stay fast; a column that fails or takes more than 200 ms for a screenful is left blank.

A sort function returns a key: numbers sort before text (compared ignoring case), and `nil` sorts last; ties are ordered by name. Folders-first and reverse order still apply. Keys are worked out for the whole listing each time it changes, within two seconds. Plugin sort orders do not apply to the tree view.

```lua
strata.column({ name = "lines", width = 6, fn = function(entry)
  if entry.is_dir or entry.size > 1e6 then return nil end
  local n = 0
  for _ in io.lines(entry.path) do n = n + 1 end
  return tostring(n), "muted"
end })

strata.sort({ name = "tagged", title = "Tagged first", fn = function(entry)
  return #entry.tags > 0 and 0 or 1
end })
```

**Helpers**

| Function | Description |
| --- | --- |
| `strata.shell(cmd, cwd?) -> stdout, code, stderr` | Run a command and capture its output |
| `strata.which(program) -> bool` | Check whether a program is on `PATH` |
| `strata.mkdir(path) -> bool` | Create a directory and its parents |
| `strata.config_dir()` / `strata.data_dir()` | Where to read config and keep state |
| `strata.version`, `strata.platform` | strata version and OS name |

Lua's standard `io`, `os` and `string` libraries are available too. Callbacks run on the UI thread, so keep them fast; `before_*` handlers, columns and sort orders are stopped when they take too long. `statusline` and `panel` callbacks run every couple of seconds, so cache any expensive results.

Errors in a plugin show up as notifications and never crash strata. See [`plugins/`](../plugins) for the official plugins, which are good examples to copy from.
