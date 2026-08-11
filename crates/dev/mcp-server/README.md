# lumen-mcp-server

Standalone Model Context Protocol (MCP) server that bridges Claude Code (or
any MCP client) to a running Lumen app. It lets the model inspect entities,
components, resources, recent messages, and a rendered screenshot of a live
app over stdio.

## How it works

1. Your Lumen app installs the `LumenMcpPlugin` from the `lumen-mcp` crate.
   The plugin binds a localhost TCP listener (default port `7878`) and
   speaks line-delimited JSON-RPC 2.0.
2. `lumen-mcp-server` is launched by Claude Code over stdio. It speaks MCP
   on stdio and proxies tool calls to the in-app TCP server.

```
                 stdio (MCP)              TCP (JSON-RPC)
 Claude Code  <----------------->  lumen-mcp-server  <---------->  Lumen app (LumenMcpPlugin)
```

## Install

Build the binary inside the Lumen workspace:

```sh
cargo build --release -p lumen-mcp-server
# binary at: target/release/lumen-mcp-server
```

Then add the server in Claude Code. Two equivalent options:

### Option A: project-scoped `.mcp.json`

Put this file at the root of any project you want the integration available in:

```json
{
  "mcpServers": {
    "lumen": {
      "command": "/absolute/path/to/lumen/target/release/lumen-mcp-server",
      "args": ["--host", "127.0.0.1", "--port", "7878"]
    }
  }
}
```

### Option B: user-level `~/.claude.json`

```json
{
  "mcpServers": {
    "lumen": {
      "command": "/absolute/path/to/lumen/target/release/lumen-mcp-server",
      "args": []
    }
  }
}
```

The server binds lazily to the TCP port when the first tool is called, so it
registers cleanly even when no Lumen app is running yet. Until you launch one
it reports `"lumen app not running on 127.0.0.1:7878 - start your Lumen
example with LumenMcpPlugin installed"`.

## Use

Start any Lumen app that has `LumenMcpPlugin` installed. A windowed
`lumenc run` installs it by default:

```sh
cargo run -p lumenc -- run apps/scroll-tiles
```

The window opens and TCP `7878` binds; the run prints the port and a
ready-to-paste `.mcp.json` snippet on stdout. A `--headless` run leaves the
server off unless the app's `lumen.toml` asks for it with `[mcp] simulate =
true` or `[runtime] mcp = true`, and `[mcp] port = 0` disables it everywhere.

Sanity-check from a shell:

```sh
printf '%s\n' '{"jsonrpc":"2.0","method":"lumen.tick","id":1}' | nc 127.0.0.1 7878
```

Expected output. `frame` is the frame counter; `last_tick_micros` is the most
recent tick's wall-clock duration, covering the whole main schedule plus
extract and scene encode on ticks that rendered:

```
{"id":1,"jsonrpc":"2.0","result":{"frame":1234,"last_tick_micros":420}}
```

## Tools

| MCP tool | Backed by |
|---|---|
| `lumen_tick` | `lumen.tick` |
| `lumen_list_entities` | `lumen.list_entities` |
| `lumen_snapshot_text` | `lumen.snapshot_text { max_lines?, cursor?, omit_invisible? }` |
| `lumen_snapshot_tree` | `lumen.snapshot_tree { max_nodes?, omit_invisible? }` |
| `lumen_find` | `lumen.find { by_text?, by_role?, by_id?, limit? }` |
| `lumen_element_at` | `lumen.element_at { x, y }` |
| `lumen_inspect_entity` | `lumen.inspect_entity { id }` |
| `lumen_signals` | `lumen.signals { filter?, max? }` |
| `lumen_set_signal` | `lumen.set_signal { name, value }` (write side; see below) |
| `lumen_lint` | `lumen.lint` |
| `lumen_diff_since` | `lumen.diff_since { tick? }` |
| `lumen_framework_status` | `lumen.framework_status` |
| `lumen_list_extracted` | `lumen.list_extracted` |
| `lumen_resources` | `lumen.resources` |
| `lumen_recent_messages` | `lumen.recent_messages { type, max? }` |
| `lumen_screenshot` | `lumen.screenshot { highlight_ids?, highlight_lint?, include_bounds_map? }` |
| `lumen_simulate` | `lumen.simulate { kind, ... }` (opt-in via `[mcp] simulate = true`) |

`lumen_recent_messages` types: `PointerMoved`, `PointerPressed`,
`PointerReleased`, `ClickEvent`, `KeyPressed`, `KeyReleased`, `MouseWheel`,
`FocusedKey`.

### `lumen.snapshot_tree`

Structured JSON element tree. Reach for it when you need hierarchy or markup
identity (tag, `#id`, `.classes`); `lumen.snapshot_text` is cheaper for
orientation.

- **Params**: `{ max_nodes?: int (default 2000, cap 10000),
  omit_invisible?: bool (default false) }`
- **Result**: `{ summary, frame, tree: [Node], total, truncated }` where
  `Node = { id, tag?, lumen_id?, classes: [string], role, label, text?,
  rect: { x, y, w, h }, flags, children: [Node] }`.
  `rect` is the scroll-corrected on-screen rect in logical pixels (the
  same space `lumen.find` and `lumen.element_at` report); `flags` is the
  Hovered / Focused / Pressed / Tab-stop string.

### `lumen.signals`

Read-only listing of every global reactive signal (PropertyStore cell).

- **Params**: `{ filter?: string (case-insensitive substring on the
  name), max?: int (default 500) }`
- **Result**: `{ summary, signals: [{ name, value, kind, generation,
  last_changed_frame }], total, truncated, frame }`. `kind` is the
  stored variant (`str | bool | i64 | f64 | color | vec2 | custom`);
  `last_changed_frame` is the snapshot frame the cell's generation last
  bumped at (`0` = never observed changing).

### `lumen.set_signal`

The one write-side tool. It routes the value through the cross-thread
external-property bus, the same ingress `Signals::set` mirrors through, so the
write commits at a tick boundary with script-write ordering semantics intact.
It is always enabled, unlike `lumen.simulate`.

- **Params**: `{ name: string, value: string | number | bool }`. The
  value is written as the canonical string variant (`true` / `false` for
  bools, decimal repr for numbers), matching what `Signals::set`
  produces, so `bind-text` labels and `<if eq>` comparators behave
  identically.
- **Result**: `{ summary, name, value, committed, observed_value,
  frames_waited }`. `committed: true` means the write was observed in a
  fresh snapshot within 500 ms. On windowed apps the snapshot refreshes
  at 1 Hz, so `committed: false` there usually means "unconfirmed" rather than
  "failed"; headless apps snapshot every tick and confirm immediately.

## Configuration

| Flag | Env | Default |
|---|---|---|
| `--host <HOST>` | `LUMEN_MCP_HOST` | `127.0.0.1` |
| `--port <PORT>` | `LUMEN_MCP_PORT` | `7878` |
