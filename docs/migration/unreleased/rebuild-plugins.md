# Rebuild portable plugins and compiler plugins

Both kinds of native plugin record the engine version they were built
against, and both of those versions moved in this release.

Portable plugins (the `lumen-plugin` cdylibs an app lists under
`[dependencies]`, registry packages included) are checked against the script
wire version. It moved because an `http()` request now carries a
`credentials` option. A plugin built for the previous release fails its
handshake: the app prints a banner naming the plugin and starts without it.

Compiler plugins (`[[plugins]]` in `lumen.toml`, built with `lumenc-plugin`)
are checked against the compiled-app format, which moved as well, so `lumenc`
refuses one built for the previous release.

Rebuild each plugin against this release's `lumen-plugin` or `lumenc-plugin`
crate. If you publish one to the registry, publish the rebuilt library as a
new version.
