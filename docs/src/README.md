# Lumen

Lumen is a markup-first UI framework for native desktop apps. You author
a `.lmn` markup tree (XML-shaped, fixed tag vocabulary), an optional
`.css` subset stylesheet, and an optional `.rhai` script — the
`lumenc` compiler parses all three to a layout IR, spawns ECS entities
across a two-world bevy_ecs core (main + render), and renders the
result through vello + wgpu with cosmic-text shaping and taffy flexbox.

```xml
<root>
  <column padding="24" gap="12">
    <label id="hello" text="Hello, Lumen!" />
    <button id="bump" text="Click me" />
  </column>
</root>
```

```rhai
signal("clicks", 0);
derive("hello", ["clicks"], |n| "Clicks: " + n);
on("click", "bump", "bump");
fn bump(_id) { let c = signal("clicks", 0); c.set(c.get() + 1); }
```

> **Status.** Alpha. The author-facing surface — markup tags, the CSS
> subset, and the Rhai builtins — is stable and documented in this book.
> FFI and multi-window are not yet wired; they have their own designs
> and land later.

## What's in the box

**Shipped**

- Two-world ECS (main + render) with an explicit extract boundary and a
  layout IR.
- Markup parser, CSS subset, and Rhai scripting host.
- Templates: `<template>` + `<slot>`, id auto-namespacing, default
  attribute values.
- Animations: CSS `transition:` (opacity) plus a generic `Transition<T>`
  primitive for programmatic tweens.
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
- Performance: frame-dirty roll-up, retained extract, and a sub-scene
  cache.
- Tooling: an LSP with completion / hover / diagnostics, `fmt --check`,
  criterion benches, and this mdBook.

**In progress**

- Virtualized `<for>` lists — the row-pool shape works but scroll-window
  math is still hardcoded.
- `pattern` validation matches a literal substring; a full regex backend
  is not yet wired.
- Transition drivers beyond `opacity` (`bg`, `color`, `radius`).
- C-ABI FFI surface — stub-only today (`lumen/ffi` is a status enum).
- Multi-window — an app is one window for now.

The widget garden app at [`apps/widget-garden`](https://github.com/lumen-ui/lumen/tree/main/apps/widget-garden)
exercises every shipped tag, attribute, and OS-integration builtin in a
single file. Use it as the canonical reference when something in the
docs is ambiguous.

## How to read this book

- **Getting started** — install, run your first app, what a project
  directory looks like.
- **Authoring** — exhaustive references for the markup tag set, the
  CSS subset, the Rhai builtins, templates / slots, animations, and
  per-app config.
- **Reference** — links to the root [SDD](https://github.com/lumen-ui/lumen/blob/main/docs/SDD.md)
  + UI API plan plus author guides for plugins and the C-ABI.

## CLI surface

| Command | What it does |
|---|---|
| `lumenc run <dir>` | Launch the app in `<dir>/main.lmn`. Watches for file changes; hot-reloads markup, CSS, and Rhai. |
| `lumenc check <dir>` | Parse without spawning a window. CI gate — exits non-zero on any parse error. |
| `lumenc fmt <file>` | Reformat a `.lmn` file in place. `--check` exits non-zero on any diff (CI gate). |
| `lumenc new <template> <name>` | Scaffold from `hello`, `counter`, or `form`. |

## Source code

Repo: <https://github.com/lumen-ui/lumen>. MPL-2.0 licensed.

## Build this book

```bash
cd docs
mdbook serve --open
```

`mdbook build` writes static HTML to `docs/book/`.
