# Devtools

Lumen ships an in-window devtools panel: an overlay that draws over your
running app and shows its live element tree, global signals, tick
performance, and captured network activity, without leaving the app
window or wiring up a separate tool. Toggle it while you develop to see
what your UI and state actually look like, the way a game engine's
built-in debug overlay works rather than a standalone inspector app.

## Enabling it

The overlay lives in the `lumen-devtools` crate and is off by default -
a release or `--bundle` build never links it. Build `lumenc` with the
`devtools` cargo feature to include it:

```sh
cargo build -p lumenc --features devtools
```

(`devtools` is a weak feature that only takes effect alongside `dev-run`,
which is already in `lumenc`'s default feature set, so this is the whole
command - no `--no-default-features` juggling needed.)

## Opening it

Press **F12** while the app window has focus to toggle the panel. It
starts hidden. For a headless run, where there is no keyboard, set
`LUMEN_DEVTOOLS_OPEN` to any non-empty value other than `0` to have it
open automatically at startup instead of waiting for F12.

## What it shows

The panel docks to the right edge of the window and has three tabs,
switched by clicking:

| Tab | What it shows |
|---|---|
| **Elements** | The live element tree as indented text: markup tag, `#id`, `.classes`, on-screen size, and `hover` / `focus` / `press` flags. The overlay's own subtree is excluded, so it never inspects itself. Capped at a few hundred lines so a very large tree does not stall the redraw. |
| **Signals + Perf** | The current frame number, last tick duration, and entity count, followed by every global reactive signal as `name = value (kind)`. |
| **Network** | Every `fetch()` / `http()` call scripts have made: method, URL, correlation tag, and status (or the transport error), oldest first, capped at a bounded ring so a chatty app cannot grow it without limit. |

The body text rebuilds every tick while the panel is visible, reading the
same in-process snapshot the MCP introspection tools use - it opens no
socket of its own.

## What is not built yet

The panel is read-only and text-based today. It has no click-to-select
node with a full component dump, no way to write a signal from the
panel, no on-screen highlight for a selected element, and no search or
filter box on the Elements tab. Tab clicks and F12 are the entire
interaction surface.

## Driving an app instead of watching it

To query or control a running app from a script, CI job, or AI agent
rather than watching the overlay live, use the MCP JSON-RPC tools: the
`lumenc snapshot` / `screenshot` / `click` / `type` / `key` / `scroll`
CLI commands, or the `lumen-mcp-server` bridge. See
[Headless mode](headless.md) for how that server is enabled, and
`lumen/mcp-server/README.md` for the full tool list and schemas.
