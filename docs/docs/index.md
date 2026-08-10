# Lumen

Lumen is a markup-first UI framework for native desktop apps. You describe the
interface in `.lmn` markup, style it in CSS, and drive it with a script. The
result is a native application on Linux, macOS, and Windows, drawn on the
GPU.

```html
<root padding="32" gap="20" align="center" justify="center">
  <label id="counter" text="0" bind-text="clicks" font-size="96" />
  <button id="bump" text="+1" width="120px" height="48px" />
  <script src="main.cdl" />
</root>
```

[Install it](getting-started/install.md), then
[build the counter](getting-started/first-app.md).

## Who it is for

Lumen suits you if you want a desktop app that looks and behaves like one, and
you would rather write markup and CSS than lay out widgets in code. The
languages are small, the feedback loop is a file save, and none of it requires
a Rust toolchain to use.

It is not a browser. There is no DOM engine, no JavaScript runtime, and no web
view; the CSS is a focused subset aimed at application UI rather than
documents. If you need to render arbitrary web content, embed a browser
instead.

## What you get

**Markup and layout.** A compact tag set covering containers, text, images,
buttons, text fields, toggles, switches, sliders, checkboxes, radios, progress
bars, tabs, dropdowns, menus, dialogs, tooltips, date and time pickers, and
scrollable regions. Layout is flexbox, with CSS grid where a two-dimensional
arrangement fits better. Reusable subtrees come from `<template>`, and larger
apps split across files with `<include>`.

**Styling.** Selectors, the cascade, specificity, pseudo-classes, and custom
properties, all behaving the way they do on the web. Skins supply a platform
look, and `transition` animates a property change without a line of script.

**Reactivity.** Named signals hold app state. Markup follows them through
`bind-*` attributes, `<for>` renders a list from an array signal, and `<if>`
mounts and unmounts a subtree. Writing a signal is the whole update; nothing
imperatively pokes at elements.

**Scripting.** candela is the default language, with Rhai and Lua available as
alternatives. Each script file's extension picks the host that runs it, and an
app can use more than one language, sharing state across them through signals.
Scripts respond to lifecycle events, input, timers, and network replies, and can
build and edit the element tree directly.

**Multi-page apps.** Every `.lmn` file in the directory is a page, reachable by
its filename. `<a href="settings">` navigates, a shared `layout.lmn` wraps every
page, and back and forward work.

**Text and internationalisation.** Shaping with font fallback, bidirectional
text, and selection. Fluent catalogues translate the UI, with plural rules and
right-to-left layout.

**The desktop around your app.** Native menus, tray icons, notifications,
global hotkeys, file dialogs, clipboard, drag and drop, and audio playback.

**Accessibility.** The UI is published to the platform accessibility layer
through AccessKit, so screen readers see the control tree.

**Tooling.** `lumenc` creates, checks, formats, runs, and packages apps. A
headless mode runs the full pipeline with no window for CI, with subcommands to
click, type, screenshot, and snapshot a running app. An in-window devtools
overlay, an editor language server, and an introspection server round it out;
see [tooling](reference/tooling.md).

## Known limitations

Lumen is in alpha. APIs can change between releases, so pin a version for
anything you depend on.

- An app has one window. Dialogs, popups, and overlays are drawn inside it.
- Desktop only. There is no web, iOS, or Android target.
- The CSS is a subset. There are no pseudo-elements, and animation is limited
  to transitions between property values.
- Scripts in different languages share signals but nothing else. One language's
  functions are not callable from another.
- A few OS integrations vary by platform, and each degrades rather than fails.
  Native menus are macOS and Windows; global hotkeys on Linux need an X11
  session; tray icons on some Linux desktops need a shell extension.

## How to read these docs

- **Getting started** takes you from an empty machine to a running app:
  [install](getting-started/install.md), the
  [first app](getting-started/first-app.md), what the files in an app directory
  are, and the templates you can scaffold from.
- **Guides** are task-shaped. Each one covers an area end to end, in the order
  you meet it: [markup](guides/markup.md), [styling](guides/styling.md),
  [reactivity](guides/reactivity.md), [scripting](guides/scripting.md),
  [pages](guides/pages.md), [composition](guides/composition.md),
  [animations](guides/animations.md),
  [OS integration](guides/os-integration.md),
  [internationalisation](guides/i18n.md),
  [accessibility](guides/accessibility.md), [packaging](guides/packaging.md),
  and [testing](guides/testing.md).
- **Reference** is for lookup once you know what you are doing: every
  [tag](reference/tags.md), every [CSS form](reference/css.md), every
  [config key](reference/lumen-toml.md), every
  [CLI flag](reference/cli.md), the builtins for
  [candela](reference/scripting-candela.md),
  [Rhai](reference/scripting-rhai.md), and [Lua](reference/scripting-lua.md),
  the [C ABI and SDKs](reference/ffi.md), and the
  [tooling surfaces](reference/tooling.md).
- **[Candela language](/candela/)** documents the language itself. Lumen's
  bindings live in this doc set; the syntax and standard library live there.
- **Contributing** is for working on Lumen rather than with it:
  [building it](contributing/building-lumen.md), how it
  [fits together](contributing/architecture.md), and how to
  [write a plugin](contributing/plugins.md).

Lumen is MPL-2.0 licensed and developed at
[github.com/lumen-fx/lumen](https://github.com/lumen-fx/lumen).
