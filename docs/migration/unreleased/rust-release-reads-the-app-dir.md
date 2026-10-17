# A release Rust app reads its files from the app, not the working directory

This affects Rust SDK apps built in release, and code that builds or matches
`Source::Embedded` by hand.

A release build embeds the entry markup and stylesheet, and reads everything
else (`@import`ed stylesheets, `@font-face` fonts, `url()` images,
`lumen.toml`) from the app directory. That directory used to be the working
directory, so a release binary started anywhere but its crate directory lost
its imports, fonts, images and window settings. It is now the executable's
directory when that holds a `lumen.toml` (a packaged app), else the directory
`lumen_source!` embedded the sources from, and the working directory only when
neither exists.

A release binary that relied on being started from a directory holding a
different `lumen.toml` than its own now reads its own; pass that directory with
`with_dir` instead.

`Source::Embedded` has a third field, `dir`, the app directory the sources came
from. `Source::embedded(markup, css)` leaves it empty, and the new
`Source::embedded_from(markup_file, markup, css)` records the app directory of
`markup_file`. Code that matches the variant adds `..` or the field:

```rust
// before
Source::Embedded { markup, css } => { /* ... */ }

// after
Source::Embedded { markup, css, .. } => { /* ... */ }
```
