# `EventKind` has a `Custom` variant

This affects a Rust SDK app that matches on `lumenui::EventKind` without a
wildcard arm. `EventKind::Custom(&'static str)` names an event a module
raises, such as a dock's `layoutchange`, so a handler can be registered for
it with `on(EventKind::Custom("layoutchange"), "editor", ...)`. An exhaustive
`match` stops compiling until it handles the new variant:

```rust
match ev.kind() {
    EventKind::Click => { /* ... */ }
    // ...
    EventKind::Custom(name) => { /* a module's event, by name */ }
}
```

`Event` also gains `detail()` and `default_prevented()`, which read what a
custom event carries.
