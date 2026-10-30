# The C ABI window functions take a `LumenWindow` first

This affects C, C++ and Python programs that call `lumen_window_set_title`,
`lumen_window_set_size`, or `lumen_window_dpr` directly. The ABI version is
now 0.16.

Each takes a `LumenWindow` handle as its first parameter. Pass
`LUMEN_WINDOW_CURRENT` for the window the app has:

```c
lumen_window_set_title("Notes");
lumen_window_set_title(LUMEN_WINDOW_CURRENT, "Notes");

float dpr;
lumen_window_dpr(&dpr);
lumen_window_dpr(LUMEN_WINDOW_CURRENT, &dpr);
```

Rebuild against the new `lumen.h`; a program built against 0.15 does not
relink. The C++ SDK's `lumen::window` and the Python SDK's `lumen.window` pass
the handle for you.

New in the same version: `lumen_window_title`, `lumen_window_size`,
`lumen_window_close`, and `lumen_app_quit`.
