# Lumen

Lumen is a markup-first UI framework for native desktop apps. You author
a `.lmn` markup tree (XML-shaped, fixed tag vocabulary), an optional
`.css` subset stylesheet, and a `.cdl` candela script; `lumenc` compiles
the three into one app and runs it on the GPU with real text shaping and
flexbox layout.

```xml
<root>
  <column padding="24" gap="12">
    <label id="hello" text="0" bind-text="clicks" />
    <button id="bump" text="Click me" />
  </column>
  <script src="main.cdl" />
</root>
```

```candela
import "lumen.cdl";

fn on_ready() {
    let bump = document::get_by_id("bump");
    lumen::event_on(bump, "click", "on_bump");
}

fn on_bump(ev) {
    let n = lumen::signal_get_int("clicks") + 1;
    lumen::signal_set_int("clicks", n);
    let target = lumen::event_target(ev);
    lumen::node_class_add(target, "hot");
}

fn main() {}
```

candela is Lumen's scripting language. One `import "lumen.cdl";` gives a
script the whole host surface: signals, the DOM API, timers, dialogs, and
the OS integrations. The language itself (syntax, types, standard library)
is documented at <https://candela.lumenfx.dev/>; these docs cover the
Lumen bindings. Rhai (`.rhai`) and Lua (`.lua`) hosts ship as well and
expose the same builtins.

> **Status.** Alpha. The author-facing surface - markup tags, the CSS
> subset, and the script builtins - is stable and documented in these
> docs. Multi-window is not wired yet; an app is one window.

## What's in the box

**Shipped**

- Two-world ECS (main + render) with an explicit extract boundary and a
  layout IR.
- Markup parser, CSS subset, and three script hosts: candela, Rhai, Lua.
- Dynamic DOM API: query the live tree by id or selector, walk parents /
  children / siblings, spawn and move and remove elements, read back
  attributes and computed style, and bind events with capture and
  bubble phases.
- Templates: `<template>` + `<slot>`, id auto-namespacing, default
  attribute values.
- Animations: CSS `transition:` for `opacity`, `background-color`,
  `color`, and `border-color`, plus a generic `Transition<T>` primitive
  for programmatic tweens.
- Rich visuals: multi-shadow and inner shadows, linear / radial / conic
  gradients, and multi-line ellipsis truncation.
- Widgets: `<dropdown>`, `<menu>`, `<textarea>`, `<tabs>`, `<dialog>`,
  `<tooltip>`, and validated `<date-picker>` / `<time-picker>`.
- OS integration: native menu bar, system tray, notifications, global
  hotkeys, file dialogs, and multi-monitor awareness.
- Form validation: `required`, `pattern`, `min`, `max`.
- CSS cascade: specificity weighting, descendant / child combinators,
  structural pseudos, `:is()` / `:where()` / `:not()`, `!important`,
  and `@media` (color-scheme, reduced-motion, contrast, width) resolved
  at runtime.
- Keyboard: word-wise navigation and delete, select-all, slider and
  scroll key control, and Escape / outside-click popup dismissal.
- Virtualized `<for>` lists: `virtualized="true" row-height="N"` inside
  a `<scroll>` mounts only the rows in the visible band.
- C-ABI plus C++, Python, and Rust SDKs for embedding Lumen in a host
  application.
- Performance: frame-dirty roll-up, retained extract, and a sub-scene
  cache.
- Tooling: an LSP with completion / hover / diagnostics, `fmt --check`,
  criterion benches, and these docs.

**In progress**

- `pattern` validation matches a literal substring; a full regex backend
  is not yet wired.
- Multi-window - an app is one window for now.

The widget garden app at [`apps/widget-garden`](https://github.com/lumen-fx/lumen/tree/main/apps/widget-garden)
exercises every shipped tag, attribute, and OS-integration builtin in a
single file. Use it as the canonical reference when something in the
docs is ambiguous.

## How to read these docs

- **Getting started** - install, run your first app, what a project
  directory looks like.
- **Authoring** - exhaustive references for the markup tag set, the
  CSS subset, the scripting builtins, templates / slots, animations, and
  per-app config.
- **Reference** - the browser inspector, headless runs, plugin
  authoring, and the C-ABI.

## CLI surface

| Command | What it does |
|---|---|
| `lumenc run <dir>` | Launch the app in `<dir>/main.lmn`. Watches for file changes; hot-reloads markup, CSS, and scripts. |
| `lumenc check <dir>` | Parse and compile the script without spawning a window. CI gate - exits non-zero on any error. |
| `lumenc fmt <file>` | Reformat a `.lmn` file in place. `--check` exits non-zero on any diff (CI gate). |
| `lumenc new <template> <name>` | Scaffold from the template gallery. `lumenc new --list` prints it. |
| `lumenc build <dir> <out.lmna>` | Compile an app ahead of time into a single artifact. |

## Source code

Repo: <https://github.com/lumen-fx/lumen>. MPL-2.0 licensed.

## Build these docs

```bash
cd docs
uv run zensical serve
```

`uv run zensical build` writes static HTML to `docs/site/`.
