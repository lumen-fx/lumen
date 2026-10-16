# `ShowContextMenu` is now `ContextMenuEvent`

This affects Rust code that reads `lumen_core::input::ShowContextMenu`. The
message used to come only from an assistive technology's request. It is now
the one context-menu request every source raises: a right click, the
ContextMenu key, Shift+F10, an assistive technology, and a page's browser. It
carries where a menu belongs and which button asked.

```rust
// before
fn on_menu(mut requests: MessageReader<ShowContextMenu>) {
    for request in requests.read() {
        show_menu_for(request.entity);
    }
}
// after
fn on_menu(mut requests: MessageReader<ContextMenuEvent>, mut signals: Signals) {
    for request in requests.read() {
        signals.open_menu_at("menu", request.position.x, request.position.y);
    }
}
```

Scripts receive the same requests as the `contextmenu` DOM event.
