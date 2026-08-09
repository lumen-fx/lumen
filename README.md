<p align="center">
  <img src="assets/colored-logo.png" alt="Lumen" width="320">
</p>

# Lumen

Lumen is a UI framework for native desktop apps that you write as markup, a
stylesheet, and a script.

You describe the window as a `.lmn` tree, style it with a CSS subset, and drive
it from a script. `lumenc` compiles the three into one app and runs it on the
GPU, with real text shaping and flexbox layout. Nothing browser-shaped sits
underneath: no Electron, no embedded webview, no JavaScript engine, and the app
ships as a native binary.

Reach for Lumen if markup and CSS are how you like to build a UI and the result
has to be a desktop app.

## Install

```sh
curl -fsSL https://lumenfx.dev/install.sh | sh
```

The installer puts `lumenc` and the `liblumen` runtime under `~/.lumen` and
asks before touching your PATH. Linux and macOS, x86_64 and aarch64. To build
from a checkout instead, see
[Building Lumen from source](docs/docs/contributing/building-lumen.md).

## Your first app

```sh
lumenc new counter my-app
lumenc run my-app
```

That scaffolds a runnable directory and opens it in a window. `main.lmn` is the
tree:

```xml
<root bg="#0c1c30" padding="32" gap="20" align="center" justify="center">
  <label class="display" id="counter" width="100%" height="120px" text="0"
         bind-text="clicks" />
  <row gap="14" justify="center">
    <button class="primary" id="bump"  width="120px" height="48px" text="+1" />
    <button class="primary" id="reset" width="120px" height="48px" text="reset" />
  </row>
  <script src="main.cdl" />
</root>
```

`main.cdl` is the script:

```candela
import "lumen.cdl";

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::on("click", "bump", "handle_bump");
    lumen::on("click", "reset", "handle_reset");
}

fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn handle_reset(id) {
    lumen::signal_set_int("clicks", 0);
}

fn main() {}
```

Clicking `+1` writes the `clicks` signal. The label carries
`bind-text="clicks"`, so it re-renders itself; no code sets its text. The
scaffold ships a `main.css` and a `lumen.toml` alongside these two, and a
README describing what the template shows.

Edit the markup or the stylesheet while the app runs and the change lands
without a restart, with the running count intact. Add `--headless` to
`lumenc run` to drive the same app with no window, which is how you run one in
CI.

candela is the default scripting language. One `import "lumen.cdl";` gives a
script the whole host surface: signals, the DOM API, timers, dialogs, and the
OS integrations. Rhai (`.rhai`) and Lua (`.lua`) hosts ship as well and expose
the same builtins.

## What you get

- A fixed markup vocabulary of layout containers, text, images, and controls,
  composable through `<template>` and `<slot>`.
- A CSS subset with custom properties, specificity and combinators,
  structural and state pseudo-classes, `@media` queries, and transitions.
- A scripting surface that reads and writes signals, queries and edits the
  live element tree, and binds events by capture or bubble phase.
- Widgets you would otherwise hand-build: dropdowns, menus, dialogs,
  tooltips, tabs, text areas, and validated date and time pickers.
- Multi-page apps with no router to configure: a second `.lmn` file next to
  `main.lmn` is a second page, reachable through a plain `<a href="...">`.
- Native shell integration: menu bar, system tray, notifications, global
  hotkeys, file dialogs, clipboard, drag and drop, and multi-monitor
  awareness.
- Accessibility through AccessKit, localization through Fluent and ICU4X,
  and audio playback.
- Headless runs plus automation subcommands that snapshot, search, and click
  a running app, so a UI can be tested without a screen.
- A C ABI with C++, Python, and Rust SDKs for embedding Lumen in a host
  application, and a language server for `.lmn` files that you build from
  this repo.

## Examples

`apps/` holds working apps to read. `apps/widget-garden` exercises every
shipped tag, attribute, and OS builtin in a single file and is the reference
when a doc is ambiguous; `apps/notes`, `apps/music`, and `apps/tracker` are
full apps in candela. From a checkout:

```sh
cargo run -p lumenc -- run apps/widget-garden
```

## Status

Alpha. Every tag, CSS property, and builtin in the docs works, and any of them
can still change between releases. An app is one window; multi-window is on the
roadmap.

## Docs

Full documentation lives at <https://docs.lumenfx.dev>: install, a guided
first app, the tag and CSS references, the scripting builtins, the `lumenc`
command surface, and the C ABI.

The same pages are in `docs/`. To read them locally you need
[uv](https://docs.astral.sh/uv/):

```sh
cd docs
uv run zensical serve
```

## Contributing

Issues and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md)
first: it lists the invariants a change must not break and the lint gates CI
runs. Open an issue before building anything large, since APIs are still
moving.

## License

Mozilla Public License 2.0. See [LICENSE](LICENSE).
