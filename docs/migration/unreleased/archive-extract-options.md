# `archive::extract` takes a fourth argument, `opts`

This affects scripts in apps that declare `lumen-archive`. `extract` now takes
an options value after the tag, whose `include` field lists glob patterns for
the files to keep. The argument is required, so a three-argument call no
longer compiles in candela and raises in Rhai and Lua.

Pass the defaults to keep the previous behaviour of writing every file.

candela, before and after:

```rust
archive::extract("themes.zip", "themes", "themes");
archive::extract("themes.zip", "themes", "themes", Default::default());
```

Rhai:

```rhai
archive::extract("themes.zip", "themes", "themes");
archive::extract("themes.zip", "themes", "themes", #{});
```

Lua:

```lua
archive.extract("themes.zip", "themes", "themes")
archive.extract("themes.zip", "themes", "themes", {})
```

To keep only some files, name them, for example
`archive::ExtractOptions { include: ["*.so"] }` in candela. The pattern rules
are listed with `archive::extract` in each host's scripting reference.
