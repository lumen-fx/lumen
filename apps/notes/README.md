# Margin - Markdown notes

A two-pane markdown editor: a note list on the left, a text editor in the
middle, and a live preview on the right. Switching notes preserves every
note's text, and a theme toggle swaps the whole color scope.

## Run

```
cargo run -p lumenc -- run apps/notes
```

## What it demonstrates

- **Markdown parsed in C** - the preview's markdown is classified by a small
  C library (`md.c`, built to `libmd.so`). The candela script imports it with
  a `dylib "md"` block, splits the note body into lines, and calls
  `md_class` / `md_text` per line to get each block's CSS class and text. This
  is candela calling a bundled C library on the host runtime.
- **Element-wise DOM lists** - the sidebar note list and the preview blocks
  are built from real elements (`node_spawn` / `node_append`), not a `<for>`
  or `signal_array`. Per-row layout lives in CSS classes.
- **Event-driven live preview** - the editor's input event (`on_text_input`)
  writes the new text back into the current note and rebuilds the preview and
  sidebar. There is no tick watcher.
- **State-preserving note switching** - each note's body is a `body:<id>`
  signal; loading a note never loses the text of the one you left.
- **Theme toggle** - `set_root_class("app theme-light")` swaps between two
  full `--var` scopes declared on the `<root>` class. Every descendant reads
  the same variables, so the whole tree retints in one tick.

## The C markdown library

`md.c` is a self-contained, single-line markdown classifier: headings
(`#`..`###`), unordered list items, thematic breaks, indented code, and
paragraphs. Build it next to the app before running:

```
cc -shared -fPIC -O2 -o apps/notes/libmd.so apps/notes/md.c   # Linux
cc -shared -fPIC -O2 -o apps/notes/libmd.dylib apps/notes/md.c # macOS
```

candela resolves the bare name `md` to `libmd.so` / `libmd.dylib` in the app
directory. `libmd.so` is committed so the app runs without a separate build
step.

## Design

Two complete token scopes: an ink dark mode with a warm ochre accent, and an
ivory light mode with burnt orange. Preview headings step down a clear type
scale; the active note gets an accent title.
