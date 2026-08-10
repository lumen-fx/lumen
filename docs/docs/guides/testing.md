# Testing an app

Lumen apps can run without a window and be driven from the command line. That
gives you two things: a way to check that an app starts and stays up on a
machine with no display, and a way to click through it from a script.

## Run without a window

```sh
lumenc run myapp --headless
```

This is not a reduced mode. Layout runs, the GPU renders, scripts execute, hot
reload works, and screenshots come out pixel-identical to the windowed path.
The only thing missing is the window: no compositor is touched, so this is
safe on a build machine and safe to run while you are working.

Ticks happen on demand. The app ticks when something asks it to (an
animation, a pending change, an incoming command) and parks otherwise, so an
idle headless app costs nothing.

| Flag | Effect |
|------|--------|
| `--size WxH` | Logical viewport size. Defaults to `[window] size`, then `960x720`. |
| `--dpr N` | Scales the render target. Screenshot pixels are the logical size times this. Defaults to 1. |
| `--ticks N` | Runs exactly N ticks back to back, then exits 0. |

Ctrl+C and `SIGTERM` exit 0 through the same graceful-close path an ordinary
window close takes, so close handlers still fire.

### A smoke test

The shortest useful check is that the app builds its tree and ticks:

```sh
lumenc run myapp --headless --ticks 5
```

It exits 0 on success and non-zero on a parse error, a missing asset, or a
script that fails to compile. For a parse check alone, `lumenc check myapp` is
faster and opens nothing.

## Drive a running app

A running app can answer questions about its own UI and accept input, over a
local TCP port. `lumenc` ships one subcommand per operation.

Reading the UI needs nothing but a running app. Injecting input is opt in:

```toml
[mcp]
simulate = true
```

Without that key, `click`, `type`, `key`, and `scroll` refuse to run and say
so. It also matters for headless runs specifically: a headless app turns the
server off unless `simulate = true` is set, because a plain `--ticks` run has
nobody to talk to.

### Reading the UI

`lumenc snapshot` prints the live tree, one entity per line, with id, role,
label, position, size, and state:

```
$ lumenc snapshot
# 11 entities (11 shown; 11 total in snapshot)
4294967202  node                                          0,0     960x720  -
4294967201    scroll                                      0,0     960x504  -
4294967200      text     "Tile 1"                         4,8     960x80   T
```

`lumenc find` searches that tree by text, role, or id, and exits non-zero when
nothing matches, which makes it usable as an assertion by itself.
`lumenc element-at x y` answers what is under a point.

### Sending input

`lumenc click x y` clicks at a logical-pixel point and reports how many frames
the app took to settle. `lumenc type`, `lumenc key`, and `lumenc scroll` cover
text, key presses, and the wheel. Each accepts `--wait-for <ring>` to block
until the app records the matching event, for example
`lumenc click 40 20 --wait-for ClickEvent`.

### Pictures and findings

`lumenc screenshot out.png` writes a PNG. `--highlight` outlines specific
entity ids and `--lint` outlines everything the linter flagged, which turns a
screenshot into a readable bug report. `lumenc lint` prints those findings as
text and exits non-zero on an error-severity one. `lumenc diff` reports what
changed since a given tick.

### Which port

Each command resolves its port in this order:

1. `--port <n>`
2. `LUMEN_MCP_PORT`
3. `[mcp] port` in `lumen.toml`, when you pass `--app <dir>`
4. `7878`

Pass `--app myapp` when the app sets its own port, so both sides agree without
you repeating the number. Give each app its own port if you run more than one
at a time.

## A test in CI

Start the app, wait for it to come up, drive it, assert, and stop it. Put the
port and the simulate key in `lumen.toml` first:

```toml
[mcp]
port = 7999
simulate = true
```

Then:

```sh
#!/bin/sh
set -e

lumenc run myapp --headless &
APP=$!
trap 'kill $APP' EXIT

# Wait for the app to answer.
until lumenc snapshot --app myapp >/dev/null 2>&1; do sleep 0.2; done

# The button exists. `find` exits non-zero with no matches.
lumenc find --text "Add" --app myapp

# Click it, then assert the label changed.
lumenc click 100 550 --app myapp --wait-for ClickEvent
lumenc find --text "1 item" --app myapp

# Keep a picture of the failure if there is one.
lumenc screenshot result.png --app myapp
```

Every command exits 0 on success and non-zero on failure, so `set -e` is the
whole assertion mechanism. `--json` on `snapshot`, `find`, `element-at`,
`lint`, and `diff` gives you machine-readable output when you want to check
something a shell cannot.

Two things to watch for:

- Do not combine `--ticks` with the automation commands. A bounded run exits
  as soon as it hits the tick count, and your driver will find nothing to talk
  to. Leave the run unbounded and stop it yourself.
- Poll for readiness rather than sleeping a fixed time. Startup includes GPU
  bring-up and a font scan, and both vary by machine.

## Static checks

Three checks need no running app and belong in the same CI job:

```sh
lumenc check myapp
lumenc fmt myapp/main.lmn --check
lumenc lint --signals myapp --strict
```

`check` parses the app. `fmt --check` fails when the markup is not formatted
and rewrites nothing. `lint --signals` reads the markup, the script, and the
optional `[signals]` schema, and reports untyped writes, schema mismatches,
ambiguous `{name}` interpolation, signals bound in markup with no schema
entry, and signals the script writes that nothing reads. `--strict` turns its
warnings into failures.

`lumenc lint --css-cascade myapp` is worth running once when you inherit an
older stylesheet: it reports rules whose resolved value depends on cascade
ordering.

Full flag lists for all of these are in the [CLI
reference](../reference/cli.md).
