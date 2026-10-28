# The error banner holds a report, and a script load failure carries its error

This affects Rust code that reads or sets the in-window error banner, or that
builds `lumen_script::ScriptLoadFailure`.

`ErrorBanner` (in `lumenc::run` and `lumen_runtime::run`) now holds an
`Option<ErrorReport>` rather than an `Option<String>`. Set a plain message
with `ErrorReport::new(message)`, and point it at a file with
`.at(ErrorLocation { file, line, col })`, which also reads that line for the
banner. Read the banner's text with `report.to_string()`.

`ScriptLoadFailure` gains a second field, the host's `ScriptError` when there
was one. When you build one, pass `None` for a failure that has no script
error behind it:

```rust
app.world.insert_resource(ScriptLoadFailure(message, None));
```

Code that reads `.0` is unchanged.
