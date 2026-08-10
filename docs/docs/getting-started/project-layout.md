# Project layout

A Lumen app is a directory. Most of what is in it is found by name, so there is
little to wire up: put a file where Lumen looks for it and it is used.

## The files

- `main.lmn` - the markup. This is the only required file. `[app] entry` in
  `lumen.toml` can name a different file.
- `main.css` - the stylesheet. Optional, and picked up automatically when it is
  there. You never link it from the markup. Split it with `@import "other.css";`
  at the top of the file.
- `main.cdl`, `main.rhai`, or `main.lua` - the script. Attach it with
  `<script src="main.cdl" />` in the markup. The extension picks the language;
  `.cdl` is candela, and `.rhai` and `.lua` are the other two hosts. A short
  script can live inline between `<script>` tags instead.
- `lumen.toml` - the app manifest. Window title and size, the starting locale,
  which script engine to use, build hooks, and everything else static about the
  app. Every key is listed in the
  [lumen.toml reference](../reference/lumen-toml.md).
- `README.md` - written by the scaffolder to explain what a template
  demonstrates. Delete it whenever you like; nothing reads it.

## Additional files Lumen looks for

- Other `.lmn` files are pages. Each one is reachable by its filename without
  the extension, `settings.lmn` is `/settings`, and `index.lmn` is the home
  page. `layout.lmn` is reserved: it contributes a shared template to every
  page rather than being a page of its own. A directory holding one `.lmn` file
  is a single-page app and none of this applies. See the
  [pages guide](../guides/pages.md).
- `locale/` holds translation catalogues, one `.ftl` file per language tag
  (`locale/de-DE.ftl`). Every catalogue in the directory loads at startup, and
  `[app] locale` picks the one the app starts in. See the
  [i18n guide](../guides/i18n.md).
- Images, fonts, and audio you reference by relative path from the markup or a
  script. Paths resolve against the app directory, so `icons/tray.png` means
  the `icons` directory next to `main.lmn`. Organise them however you like.
- Markup fragments pulled in with `<include src="parts/header.lmn" />`, and
  stylesheets pulled in with `@import`. Both resolve relative to the file doing
  the including, and both can nest.

## Running from anywhere

`lumenc run <dir>` takes the directory, so you can run an app from outside it:

```sh
lumenc run my-app
lumenc run .
```

While an app runs from source, edits to the markup, the stylesheet, the script,
included fragments, and the locale catalogues take effect without a restart.

## Next

- [The template gallery](templates.md).
- [Writing markup](../guides/markup.md) and [styling it](../guides/styling.md).
- [Packaging an app](../guides/packaging.md) into a single artifact.
