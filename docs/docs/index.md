# Lumen

Lumen is a markup-first UI framework for native desktop apps. You write a
`.lmn` markup tree, a `.css` stylesheet, and a script; `lumenc` compiles the
three into one app and runs it on the GPU with real text shaping and flexbox
layout.

```xml
<root padding="24" gap="12" align="center">
  <label id="counter" text="0" bind-text="clicks" />
  <button id="bump" text="Click me" />
  <script src="main.cdl" />
</root>
```

```candela
import "lumen.cdl";

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::on("click", "bump", "handle_bump");
}

fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn main() {}
```

## Who it is for

Reach for Lumen if you want a desktop app with web-shaped tools (markup, CSS,
a scripting language) but without a browser runtime underneath: no Electron,
no embedded webview, no JavaScript engine. The renderer draws with GPU
primitives and the app ships as a native binary.

candela is Lumen's default scripting language. One `import "lumen.cdl";` gives
a script the whole host surface: signals, the DOM API, timers, dialogs, and
the OS integrations. The language itself is documented at
<https://candela.lumenfx.dev/>; these docs cover the Lumen bindings. Rhai
(`.rhai`) and Lua (`.lua`) hosts ship as well and expose the same surface.

## What you can build with it

**Layout and text.** Flexbox and CSS grid through taffy, real shaping through
cosmic-text, word wrap, multi-line ellipsis truncation, and logical properties
that follow an element's writing direction.

**A widget set.** Buttons, text fields, toggles and switches, sliders,
checkboxes, radio groups, progress bars, dropdowns, menus and native menu
bars, tabs, dialogs, tooltips, and date and time fields. Every visual is
reachable from CSS, so a widget retints from your stylesheet rather than a
fork.

**Styling that behaves like CSS.** Full cascade order, descendant and child
combinators, structural and state pseudo-classes, `:is()` / `:where()` /
`:not()`, `!important`, custom properties, and `@media` queries resolved live
against the OS theme. Four embedded skins (default, macOS, Windows, Linux)
give an app a platform look without writing one.

**Reactive markup.** `bind-text` and friends wire an element to a named
signal, `<for each>` renders a list from an array signal, `<if>` mounts a
subtree conditionally, and `<template>` / `<slot>` factor repeated markup out
with per-instance id namespacing.

**A live DOM from script.** Query the tree by id or selector, walk parents and
children and siblings, spawn and move and remove elements, read back
attributes and computed style, and bind events with capture and bubble
phases.

**Motion.** CSS `transition:` tweens `opacity`, `background-color`, `color`,
and `border-color` on a class flip, over a generic tween primitive plugins can
build on.

**Native integration.** Menu bars, system tray, notifications, global
hotkeys, file dialogs, clipboard, drag and drop, audio playback, and
multi-monitor awareness.

**Embedding.** A C ABI with C++, Python, and Rust SDKs, for driving a Lumen
app from a program written in another language.

**Tooling.** `lumenc` runs, checks, compiles, and packages an app; hot reload
swaps markup, CSS, and script while the app runs; an in-window devtools panel
and an MCP introspection server let you inspect and drive a running app; a
language server gives `.lmn` files completion, hover, and diagnostics.

## Known limits

Lumen is alpha, and its API is not stable yet.

- An app is one window. Multi-window is on the roadmap.
- `pattern` validation on a text field matches a literal substring; there is
  no regex backend behind it yet.
- Transitions run on entry, not on removal: hiding or closing an element is
  instant.

## How to read these docs

- **Getting started** - install the toolchain, build a counter, learn what
  belongs in an app directory, and browse the templates.
- **Authoring** - the markup tags, the CSS subset, scripting, multi-page
  navigation, templates and slots, animations, and per-app config.
- **Reference** - the CLI, devtools, headless runs, plugin authoring, the C
  ABI, and one exhaustive page per script host.

The [widget garden](https://github.com/lumen-fx/lumen/tree/main/apps/widget-garden)
app exercises every shipped tag, attribute, and OS-integration builtin in a
single file. Read it when a doc page leaves you unsure.

## Source code

Repo: <https://github.com/lumen-fx/lumen>. MPL-2.0 licensed.

## Build these docs

```bash
cd docs
uv run zensical serve
```

`uv run zensical build` writes static HTML to `docs/site/`.
