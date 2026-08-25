# Margin - Markdown notes

A markdown editor: a note list on the left, a text editor in the middle, and
a live preview on the right. Switching notes preserves every note's text, and
a theme toggle swaps the whole color scope.

## Run

```
cargo run -p lumenc -- run apps/notes
```

## What it demonstrates

- **Markdown parsed in C** - the preview's markdown is classified by a small
  C library (`md.c`, built to `lib/libmd.so`). The candela script imports it with
  a `dylib "md"` block, splits the note body into lines, and calls
  `md_class` / `md_text` per line to get each block's CSS class and text. This
  is candela calling a bundled C library on the host runtime.
- **Element-wise DOM lists** - the sidebar note list and the preview blocks
  are built element by element with `node_spawn` and `node_append`, not with
  `<for>` or `signal_array`. Per-row layout lives in CSS classes.
- **Event-driven live preview** - the editor's input event (`on_text_input`)
  writes the new text back into the current note and rebuilds the preview and
  sidebar. There is no tick watcher.
- **State-preserving note switching** - each note's body is a `body:<id>`
  signal; loading a note never loses the text of the one you left.
- **Theme toggle** - `set_root_class("app theme-light")` swaps between two
  full `--var` scopes declared on the `<root>` class. Every descendant reads
  the same variables, so the whole tree retints in one tick.

## The C markdown library

`md.c` is a self-contained, line-at-a-time markdown classifier: ATX headings
(levels 4 to 6 render as level 3), unordered list items, thematic breaks,
indented code, and paragraphs. candela resolves the bare name `md` to
`libmd.so` (Linux), `libmd.dylib` (macOS), or `md.dll` (Windows, which takes
no `lib` prefix) in the app's `lib/`.

None of those are committed; they are build artifacts, not source.
`lumen.toml` declares a `[[hooks]]` entry per OS that compiles the library
from `md.c` into `lib/` before the app runs. `lumenc run` fires it
automatically; pass `--no-hooks` to skip it:

```
mkdir -p apps/notes/lib
cc -shared -fPIC -O2 -o apps/notes/lib/libmd.so apps/notes/md.c    # Linux
cc -shared -fPIC -O2 -o apps/notes/lib/libmd.dylib apps/notes/md.c # macOS
cc -shared -O2 -o apps/notes/lib/md.dll apps/notes/md.c            # Windows
```

The Windows line assumes a MinGW-flavored `cc` (MSYS2 or MinGW-w64) is on
`PATH`; MSVC users should point `run` at `cl /LD` instead.

## Design

Two complete token scopes: an ink dark mode with a warm ochre accent, and an
ivory light mode with burnt orange. Preview headings step down a clear type
scale; the active note gets an accent title.
