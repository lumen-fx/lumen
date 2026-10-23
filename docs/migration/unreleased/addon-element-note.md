# Add-on elements carry a note

This affects Rust code that builds `lumen_ir::addon::AddonElement` with a
struct literal. The type gains `note: Option<String>`, the warning a web
build prints when a page uses the element. A module states it as `note` on
the element in its `lumen-addon.toml`:

```toml
[[element]]
tag = "surface"
note = "shows a placeholder in a browser"
```

In a literal, give it `note: None`.
