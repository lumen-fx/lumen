# Lumen C++ SDK - examples

A set of small, self-contained programs that each compile and link against
the built `lumen-ffi` cdylib and run **headless** (no window, no display) so
they double as smoke tests. They mirror the Python SDK's examples so the two
SDKs feel consistent.

| Example | Shows |
| --- | --- |
| `hello` | Smallest possible app - construct `App`, print the ABI, build-and-drop headless. |
| `counter` | Typed `Signal<int64_t>`, a `label` derived with `watch` (the `computed` analogue), and ergonomic 0-arg `on_click` handlers. |
| `signals` | Every scalar `Signal<T>` (int/double/bool/string/`Color`), the `+= / -=` operators, `Color::from_hex`, and a derived value. |
| `handlers` | `on_click` in both the 0-arg `[]{}` and 1-arg `[](std::string id){}` forms, plus a directly-registered function pointer, plus `on_close`. |
| `expose` | A native C++ builtin (`app.expose`) called from the app's Rhai `on_start`; the return value flows back over the FFI. |
| `list` | A `<for>` array signal built as `Value::array` of `Value::map` rows and pushed with `raw::set_array`. |

## Build

From this directory (`sdk/cpp/examples`):

```sh
cmake -B build
cmake --build build          # builds the cdylib if stale, then every example
```

`cmake --build build` compiles all six. To (re)build just the examples once
the library exists, use the aggregate target:

```sh
cmake --build build --target examples
```

CMake locates the workspace's cargo target directory via `cargo metadata`, so
a custom `CARGO_TARGET_DIR` is honoured, and bakes it as an RPATH - the
binaries find `liblumen_ffi.so` without `LD_LIBRARY_PATH`.

## Run

Each binary defaults to its own app directory (baked in at build time) and
runs headless:

```sh
./build/hello       # -> "app built and validated headless. OK"
./build/counter     # -> derived label over a couple of ticks
./build/signals     # -> typed reads/writes + a derived summary
./build/handlers    # -> registers 0-arg / 1-arg / fn-ptr handlers
./build/expose      # -> "script -> host dispatch confirmed. OK"
./build/list        # -> three <for> rows, read back
```

Pass a directory to point an example at a different app, and pass `--window`
(where supported: `hello`, `counter`, `handlers`) to open a real OS window
instead of running headless - native click handlers only fire in a window,
since headless mode injects no input.
