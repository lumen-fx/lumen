# Devtools - the browser inspector

Every Lumen app that runs with the MCP plugin installed (the default
`lumenc run` stack, windowed **and** `--headless`) serves a browser-based
inspector on its MCP port. It is Lumen's answer to Chrome DevTools'
element panel / Qt's GammaRay: a live view of the element tree, styles,
signals, events, and frame timing of the running app.

## Opening it

```sh
lumenc run my-app            # or: lumenc run my-app --headless
# then open:
xdg-open http://127.0.0.1:7878/
```

The port is the MCP port (`lumen.toml [mcp] port`, default `7878`;
`port = 0` disables the server and the inspector with it). The page is
served by the app itself - vanilla HTML + JS, no CDN, works offline.
Because the inspector talks to the same snapshot the MCP tools use, it
works identically against a headless instance: you can drive a windowless
CI app from a browser tab.

## Panels

| Panel | What it shows |
|---|---|
| **Elements** (left) | Live element tree, polled at 1 Hz from `lumen.snapshot_tree`. Each row shows the markup tag, `#id`, `.classes`, text label, and state flags (`H`overed, `F`ocused, `P`ressed, `T`ab-stop). Rows flash green when anything about the node changed since the last poll. The filter box narrows by tag, `#id`, `.class`, or text. |
| **Screenshot** (middle) | On-demand capture via `lumen.screenshot` (press `r` or the Refresh button; enable *auto* to capture every poll). The selected tree node's on-screen rect is drawn as a magenta overlay - scroll-corrected, dpr-independent. |
| **Inspect** (right, top) | Full component dump of the selected node from `lumen.inspect_entity`: transform, style, visuals, text style, bindings, tab index, scroll state, interaction tints. |
| **Signals** (right, middle) | Every global reactive signal from the PropertyStore: name, value, stored kind, and the frame it last changed (`@N`). Rows flash on change. Click a row to load it into the write boxes; **Set** (or Enter) writes the value back through `lumen.set_signal` - the same external-property bus `Signals::set` uses, so ordering semantics against script writes hold. |
| **Events** (right, bottom) | Tail of one message ring (`ClickEvent`, `KeyPressed`, `MouseWheel`, ...) via `lumen.recent_messages`. Newest first. |
| **Perf strip** (top bar) | Frame counter, last tick duration in us, and a sparkline of the last ~70 samples. |

## Keyboard

The inspector is keyboard-first:

| Key | Action |
|---|---|
| `/` | Focus the tree filter |
| `Up` / `Down` | Move the tree selection |
| `Left` / `Right` | Collapse / expand the selected node |
| `r` | Take a screenshot |
| `s` | Focus the signal-write box |
| `Escape` | Leave any text field |

## Under the hood

The page polls `POST /rpc` with plain JSON-RPC. Everything it does is
available to scripts and agents directly:

```sh
curl -s -X POST http://127.0.0.1:7878/rpc \
  -d '{"jsonrpc":"2.0","method":"lumen.snapshot_tree","id":1}'
curl -s -X POST http://127.0.0.1:7878/rpc \
  -d '{"jsonrpc":"2.0","method":"lumen.signals","params":{"filter":"vol"},"id":2}'
curl -s -X POST http://127.0.0.1:7878/rpc \
  -d '{"jsonrpc":"2.0","method":"lumen.set_signal","params":{"name":"volume","value":"0.5"},"id":3}'
```

The write path is one-way-safe: `lumen.set_signal` enqueues onto the
cross-thread external-property bus and wakes the (possibly parked) event
loop; the write commits at the next tick boundary, never mid-schedule.
Watch out on windowed apps: the snapshot refreshes at 1 Hz there, so the
inspector's view can trail the app by up to a second (headless instances
snapshot every tick).

See also the machine-facing surface these panels are built on:
`lumen/mcp-server/README.md` documents every MCP tool and its schema.
