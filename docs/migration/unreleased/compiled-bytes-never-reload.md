# An app started from compiled bytes no longer reloads from a `src/` beside it

This affects C ABI embedders that start an app with
`lumen_app_new_from_lmna`, and code that builds `RunOptions` with
`artifact_bytes`.

An app started from compiled bytes used to install the hot-reload watcher
when hot reload was on, and with a `src/` beside it could replace the
compiled UI with whatever it found there. It now keeps the UI it was started
with. A run that wants the reload sets the new `RunOptions::watch_sources`,
which watches the app directory's `src/` and reloads from it on an edit: the
Rust SDK sets it in a debug build.
