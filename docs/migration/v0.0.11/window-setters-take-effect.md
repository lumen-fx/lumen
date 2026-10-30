# `window.set_title` and `window.set_size` change the window

This affects scripts, C programs, and Rust SDK apps that call
`window.set_title` or `window.set_size` (`window::set_title` and
`window::set_size` in candela, `lumen_window_set_title` and
`lumen_window_set_size` in the C ABI, `dom::window::set_title` and
`dom::window::set_size` in the Rust SDK).

They used to change only what `window.title()` and `window.size()` read back;
the window on screen kept its title and size. They now change the window.
The getters report what the window has, so they read the new title or size on
a later tick, once the window has it, rather than straight after the call. A
size the window manager refuses or adjusts reads back as the size the window
got. A maximized window is restored to take the new size. A size with a side
that is not positive is ignored, and the C ABI returns `LUMEN_ERR_BAD_ARG` for
it.

An app that called these and relied on the window not changing should drop
the calls. An app that read a size back in the same handler that set it should
read it on a later tick, for example from a timer:

```rust
fn on_ready() {
    window::set_size(640.0, 480.0);
    lumen::set_timeout("resized", 100);
}

fn on_timer(name: string) {
    let size = window::size();
}
```

Two calls are new on every surface: `window.close()` closes the window through
the same `on_close` veto its close button runs, and `app.quit()` quits the app.
In Rhai and Lua, `app` is now a predefined name.
