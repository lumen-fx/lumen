# A module's markup tags come from its lumen-module.toml, not a `tags` key

This affects apps that list `tags` on a `[dependencies]` entry, and authors of
runtime modules. A module now lists the markup tags it brings in a
`lumen-module.toml` at its root, which every module ships, and every build
reads it without loading the module. An app that declares the module gets
its tags with nothing more to write, and a `tags` key left on an entry is an
error.

Drop the key from the app:

```toml
# before
[dependencies]
lumen-canvas = { bundled = true, tags = ["canvas"] }

# after
[dependencies]
lumen-canvas = { bundled = true }
```

A module author adds the file at the module's root: the directory a `path`
entry names or the one the library it names sits in, and the root of a
published package. A module that brings no markup writes an empty list.

```toml
[module]
tags = ["sparkline"]
```

A declared module without this file stops the build with an error naming
it. A web half's `[[element]]` entries keep only the HTML each tag is written
as, and an element naming a tag the module's `lumen-module.toml` does not
list is an error.

In Rust, `lumen_modules::DepCfg` loses its `tags` field, and
`DependenciesCfg::declared_tags`, `register_declared_tags` and
`register_addon_elements` are replaced by `lumen_modules::module_tags`, which
reads the descriptors, and `register_module_tags`. `lumen_runtime::CompileDeps`
gains `tags`, the list a compile publishes; build it with
`CompileDeps::new(target)` and the struct update syntax.
