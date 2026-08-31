# Lumen C++ SDK - examples

[![ci](https://github.com/lumen-fx/lumen/actions/workflows/ci.yml/badge.svg)](https://github.com/lumen-fx/lumen/actions/workflows/ci.yml)
![C++17](https://img.shields.io/badge/C%2B%2B-17-blue)
[![license](https://img.shields.io/badge/license-MPL--2.0-blue)](https://github.com/lumen-fx/lumen/blob/main/LICENSE)

Small, self-contained programs that each compile and link against the built
`lumen` shared library and run headless, with no window and no display, so they
double as smoke tests. They mirror the Python SDK's examples so the two SDKs
feel consistent.

| Example | Shows |
| --- | --- |
| `hello` | Smallest possible app: construct `App`, print the ABI, build and drop headless. |
| `counter` | Typed `Signal<int64_t>`, a `label` derived with `watch` (the `computed` analogue), and 0-arg `on_click` handlers. |
| `signals` | Every scalar `Signal<T>` (int, double, bool, string, `Color`), the `+=` and `-=` operators, `Color::from_hex`, and a derived value. |
| `handlers` | `on_click` in both the 0-arg `[]{}` and 1-arg `[](std::string id){}` forms, plus a directly-registered function pointer, plus `on_close`. |
| `expose` | A native C++ builtin (`app.expose`) called from the app's Rhai `on_start`; the return value flows back over the FFI. |
| `list` | A `<for>` array signal built as `Value::array` of `Value::map` rows and pushed with `raw::set_array`. |
| `dom` | The `lumen::dom` surface: build a detached subtree, set attributes, classes, text, and `inner_markup`, and bind a click listener. |

## Build

From this directory (`sdk/cpp/examples`):

```sh
cmake -B build
cmake --build build          # builds the library if stale, then every example
```

To rebuild just the examples once the library exists, use the aggregate
target:

```sh
cmake --build build --target examples
```

CMake locates the workspace's cargo target directory via `cargo metadata`, so
a custom `CARGO_TARGET_DIR` is honoured, and bakes it as an RPATH so the
binaries find `liblumen.so` without `LD_LIBRARY_PATH`.

## Run

Each binary defaults to its own app directory, baked in at build time, and
runs headless:

```sh
./build/hello       # -> "hello: app built and validated headless. OK"
./build/counter     # -> derived label over a couple of ticks
./build/signals     # -> typed reads/writes + a derived summary
./build/handlers    # -> registers 0-arg / 1-arg / fn-ptr handlers
./build/expose      # -> "expose: script -> host dispatch confirmed. OK"
./build/list        # -> three <for> rows, read back
./build/dom         # -> a subtree built and edited through the DOM API
```

Pass a directory to point an example at a different app. `hello`, `counter`,
`handlers`, and `dom` also take `--window` to open a real OS window instead of
running headless. Native click handlers only fire in a window, since headless
mode injects no input, and the `dom` snapshot reads (query, rect, computed
style) only return live data with a window up.
