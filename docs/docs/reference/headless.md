# Headless mode (automation / CI)

`lumenc run <dir> --headless` runs the **full** app pipeline - layout,
real GPU rendering, the MCP introspection server, input simulation,
screenshots, and hot reload - with **zero windows**. No winit event loop
is created and the compositor is never contacted, so automated
verification (AI agents, CI jobs, golden-image checks) never disturbs
the desktop the developer is working on.

```sh
lumenc run apps/widget-garden --headless
lumenc run apps/kanban --headless --size 1280x800 --dpr 1.5
lumenc run apps/counter --headless --ticks 120   # bounded CI run
```

## Flags

| Flag | Meaning |
|------|---------|
| `--size WxH` | Logical viewport size. Defaults to `lumen.toml [window] size`, else 960x720 - the same resolution order as the windowed run. |
| `--dpr N` | Device pixel ratio of the offscreen target. Screenshots come out at `logical x dpr` physical pixels, matching a windowed run on an equivalent display. Default `1.0`. |
| `--ticks N` | Run exactly `N` ticks back-to-back, fire the close path, and exit. For bounded CI validation runs. |

## What is identical to a windowed run

* Build hooks: `lumen.toml`'s `[[hooks]]` `prebuild` and `prerun` commands
  run before a headless launch exactly as they do before a windowed one -
  see [`[[hooks]]`](../authoring/lumen-toml.md#hooks). `--no-hooks` skips
  them either way.
* The tick schedule, plugin stack, scripts, bindings, and reconcilers -
  headless reuses the exact app construction the windowed path uses.
* Rendering: the same offscreen wgpu + vello renderer and retained-scene
  walker as the windowed backend (adapter requested without a surface;
  falls back to lavapipe/llvmpipe when no GPU is reachable), including
  dpr scaling and the cosmic-text shaper.
* Frame pacing semantics: tick + render when work is pending (a simulate
  event, dirty state, an active animation, a screenshot request), idle
  otherwise. An MCP request wakes a parked loop immediately, exactly like
  the windowed event-loop proxy.

## Exit

`SIGINT` / `SIGTERM` (and the end of a `--ticks` run) write a
`CloseRequest` onto the message bus, run one final tick so
close-observing systems fire, and exit with status 0.

## Documented divergences from the windowed run

* **The MCP server is off by default.** A windowed run starts the MCP
  introspection server automatically; a headless run does not, since the
  server thread and per-tick snapshot pipeline are overhead a plain
  `--ticks N` bench does not need. Set `[mcp] simulate = true` in
  `lumen.toml` to turn it on - this is also what lets `lumenc click` /
  `type` / `key` / `scroll` inject input - or set `[runtime] mcp = true`
  to force it on for introspection alone. `[mcp] port = 0` disables it
  either way. Once the server is running, `lumenc snapshot` and `lumenc
  screenshot` (real rendered PNGs) behave exactly as they do windowed.
* **No pause-on-unfocus.** The windowed scheduler suppresses redraws
  while unfocused/occluded; headless has no focus, so ticks always run
  on demand.
* **Animation pacing is wall-clock, not vsync.** Frames with pending
  animation work are paced at ~60 Hz by a 16 ms sleep instead of the
  display's vblank.
* **MCP snapshots refresh every tick** instead of the windowed 1 Hz
  throttle. Ticks only run on demand, so this costs nothing at idle and
  makes `lumen.simulate`'s completion wait deterministic.
* **Close vetoes are not honoured on signals.** A signalled automation
  run must terminate; the close tick still runs so cleanup hooks fire.
* **Window-state persistence (`[window] remember_state`) is skipped** -
  there is no window geometry to save, and clobbering the real window's
  saved state would be wrong.
* **Idle hot-reload polling.** When hot reload is active the parked loop
  wakes ~4x/second to poll source mtimes (a windowed app only polls on
  event-driven ticks).
