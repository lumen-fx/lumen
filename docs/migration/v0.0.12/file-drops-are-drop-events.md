# File drops arrive as the DOM `drop` event; `on_file_dropped` is deprecated

A file dropped on a drop target is now a `drop` event carrying
`text/uri-list`, the same event an in-app drag raises, and `on_drop` receives
the file's uri (`file:///home/me/shot.png`) instead of an empty string.
`on_file_dropped(id, path)` still fires with the plain path in this release
and is removed in the next one. Move a handler over:

```rust
// before
fn on_file_dropped(id: string, path: string) { open_image(path); }

// after
fn on_zone_drop(ev: int) {
    open_image(lumen::event_get_data(ev, "text/uri-list"));
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("zone"), "drop", "on_zone_drop");
}
```

`accept` on a drop target is now a comma-separated list with `type/*`
wildcards, as HTML `<input accept>` spells it. A single type reads as before.
A target whose `accept` names a type other than `text/plain` used to refuse
every in-app drag; it now takes a drag whose `drag-type` matches, and a drag
lights `:drag-over` only on targets that take it.

An event's `x` / `y` (`event_x`, `ev.x()`, `LumenEvent.local_x`) inside a
scrolled `<scroll>` is now measured from the element as it is painted. A
handler that subtracted the scroll offset itself should stop doing so.

Rust code that writes `lumen_os_dnd::DragStarted` or `DropAccepted` with a
struct literal gives the new `position` field, the pointer's window position:

```rust
app.world.write_message(DropAccepted {
    source: None,
    target,
    payload: "card-3".into(),
    effect: DropEffect::Copy,
    position: glam::Vec2::new(40.0, 12.0),
});
```
