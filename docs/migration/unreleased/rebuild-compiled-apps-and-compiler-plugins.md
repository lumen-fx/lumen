# Rebuild every `.lmna` artifact and every compiler plugin

The compiled-app format moved on: a tag's defaults now travel as rules in the
artifact's stylesheet rather than as values in its tree, an element with a
custom widget tag keeps the attributes it was written with, and an `<input>`
keeps the `type` it was written with. The runtime refuses
an artifact written by an earlier toolchain, which would lose those defaults
the first time an element restyled and hand its widgets no attributes.

This affects you if you load a precompiled app (`lumenc run --artifact`, a
`.lmna` written by `lumenc build`, or `lumen_app_new_from_lmna` from the C ABI
or an SDK), or if your app declares compiler plugins under `[[plugins]]`.
Loading an old artifact fails with an error that says:

```
unsupported artifact version 16 (this build reads 19)
```

Rebuild the artifact with the new `lumenc`:

```sh
lumenc build myapp myapp.lmna
```

A compiler plugin records the same format version, so `lumenc` refuses one
built for the previous release; rebuild it against this release's
`lumenc-plugin` crate. A plugin that adds elements in `transform_ir` gives
them their styles with `lumen_ir::css::apply_markup_declaration`; see the
plugin guide.

Apps you already packaged carry their own runtime and keep working as they
are, as does a deployed site from `lumenc web`.
