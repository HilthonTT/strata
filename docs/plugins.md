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
| `view` | `files`, `dashboard`, `docker` or `nas` |
| `theme` | active theme name |
| `scheme` | `local`, `sftp` or `docker` |
| `icons` | whether Nerd Font icons are enabled |

## API

**Registration**

| Function | Description |
| --- | --- |
| `strata.map(keys, fn(ctx), desc?)` | Bind a key sequence such as `"g p"` or `"ctrl+e"` |
| `strata.command(name, fn(ctx, args), desc?)` | Add a `:name` command |
| `strata.on(event, fn(ctx))` | React to events: `startup`, `cd`, `paste`, `job_done`, `quit` |
| `strata.statusline(fn(ctx) -> string?)` | Add a segment to the top bar |
| `strata.panel{ name, title?, render = fn(ctx, w, h) -> lines }` | Add a UI panel next to the file panels |
| `strata.previewer{ ext = {...}, fn = fn(path, w, h) -> lines? }` | Preview files with these extensions; return `nil` to pass |

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

**Helpers**

| Function | Description |
| --- | --- |
| `strata.shell(cmd, cwd?) -> stdout, code, stderr` | Run a command and capture its output |
| `strata.which(program) -> bool` | Check whether a program is on `PATH` |
| `strata.mkdir(path) -> bool` | Create a directory and its parents |
| `strata.config_dir()` / `strata.data_dir()` | Where to read config and keep state |
| `strata.version`, `strata.platform` | strata version and OS name |

Lua's standard `io`, `os` and `string` libraries are available too. Callbacks run on the UI thread, so keep them fast. `statusline` and `panel` callbacks run every couple of seconds, so cache any expensive results.

Errors in a plugin show up as notifications and never crash strata. See [`plugins/`](../plugins) for the official plugins, which are good examples to copy from.
