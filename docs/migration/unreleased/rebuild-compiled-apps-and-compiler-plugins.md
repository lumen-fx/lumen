# Rebuild every `.lmna` artifact and every compiler plugin

The compiled-app format moved on: a `<menu>` keeps its `menu`, `menuitem`
and `separator` elements, and a menu item carries the keyboard shortcut it
shows. The runtime refuses an artifact written by an earlier toolchain.

This affects you if you load a precompiled app (`lumenc run --artifact`, a
`.lmna` written by `lumenc build`, or `lumen_app_new_from_lmna` from the C ABI
or an SDK), or if your app declares compiler plugins under `[[plugins]]`.
Loading an old artifact fails with an error that says:

```
unsupported artifact version 21 (this build reads 22)
```

Rebuild the artifact with the new `lumenc`:

```sh
lumenc build myapp myapp.lmna
```

A compiler plugin records the same format version, so `lumenc` refuses one
built for the previous release; rebuild it against this release's
`lumenc-plugin` crate.

Apps you already packaged carry their own runtime and keep working as they
are, as does a deployed site from `lumenc web`.
