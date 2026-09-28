# Under `navigation = "hard"`, `page()` loads a new document

This affects multi-page web apps that set `[web] navigation = "hard"` and move
between pages from a script.

Before, `hard` governed links only: a script's `page()` call still swapped the
page in place, with the app and its script state still running. Now `hard`
applies to scripts too. `page()` loads the target page as its own document, as
a new history entry, and `page_back()` and `page_forward()` step the browser's
history. Script state does not carry from one page to the next.

Desktop apps, single-file apps, and `navigation = "soft"` (the default) are
unchanged.

If a page relies on state a previous page set, either keep that state where
the next document can read it, for example with the `lumen-storage` module:

```toml
[dependencies]
lumen-storage = { bundled = true }
```

```rust
storage::set_item("draft", text);   // before page("/next")
let draft = storage::get_item("draft");   // on the next page
```

or switch the site to soft navigation:

```toml
[web]
navigation = "soft"
```
