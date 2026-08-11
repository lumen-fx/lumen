# Lumen C++ SDK

Header-only C++17 bindings for the [Lumen](../../) UI framework, over the
C ABI (`include/lumen.h`). The primary surface is the typed
reactive handle `lumen::Signal<T>`: name a signal, read/write it with
natural operators, and subscribe to changes, all statically typed.

One header, no third-party dependencies: `#include <lumen.hpp>` pulls in
the C ABI header and the C++17 standard library, nothing else.

## Quickstart

```cpp
#include <lumen.hpp>
#include <string>

int main() {
    lumen::App app("counter_app", {.title = "Counter"});

    lumen::Signal<int64_t>     count{"count", 0};
    lumen::Signal<std::string> label{"label", "0 clicks"};

    app.on_click("bump", [&] {
        count += 1;                                       // typed operators
        label = std::to_string(*count) + " clicks";       // *count reads
    });

    return app.run();   // blocks until the window closes
}
```

A runnable set of examples covering the whole surface (hello, counter,
signals, handlers, expose, list, dom) lives in [`examples/`](examples/). Each
is a self-contained program that links the cdylib and runs headless; see
[`examples/README.md`](examples/README.md).

## Dynamic DOM

`lumen::dom` reaches the live element tree the way a browser script does:
query and traverse nodes, read and write attributes / classes / text /
inline style, build and rearrange subtrees, read post-layout geometry, and
bind event handlers. A `dom::Node` is a cheap handle; reads return
`std::optional` and mutations chain.

```cpp
namespace dom = lumen::dom;

dom::Node row = dom::spawn("row").add_class("item").set_text("hello");
if (auto list = dom::get_by_id("list")) list->append(row);

row.on("click", [](const dom::Event& e) { /* e.target(), e.type() ... */ });
```

`count` and `label` are typed handles over named signals: `count += 1`
writes, `*count` reads, and markup bound with `bind-text="label"` reflects
the value next tick. The `App` constructor takes a designated-initializer
`Options` struct (`{.title = ..., .width = ..., .height = ...}`); the
`title()` / `size()` builder methods remain for fluent tweaks.

## `Signal<T>` - typed reactive handles

```cpp
lumen::Signal<int64_t>      count{"count", 0};    // seed on construction
lumen::Signal<double>       ratio{"ratio"};       // or handle without seeding
lumen::Signal<bool>         flag{"flag", false};
lumen::Signal<std::string>  label{"label", "hi"};
lumen::Signal<lumen::Color> tint{"tint", lumen::Color{255, 128, 0}};

count = 10;              // operator=  (set)
int64_t n = *count;      // operator*  (get), or count.get()
count += 1;  count -= 2; // += / -= for numeric (and += for strings)

count.watch([](int64_t v) { /* fires on the tick v commits */ });
```

Supported `T`: `int64_t`, `double`, `bool`, `std::string`, `lumen::Color`
(a `static_assert` rejects anything else with a readable message).

`lumen::Color` is an aggregate: construct it component-wise
(`Color{255, 128, 0}`), with designated initialisers (`Color{.r = 255}`), or
from a CSS-style hex string with the `Color::from_hex` factory
(`Color::from_hex("#ff8000")`, `"#f80"`, `"#ff8000ff"`), the C++ parity for
the Python SDK's `Color("#ff8000")`. `to_hex()` renders back to
`#rrggbbaa`. (It's a static factory rather than a `Color(string)` constructor
precisely so `Color` stays an aggregate and the brace/designated forms keep
working.)

`watch(fn)` is an ABI subscription (`lumen_signal_watch`), not a polling loop:
`fn(new_value)` fires on the Lumen tick thread each time the value commits,
plus once with the current value on the first tick after registration. It only
fires while an app is running (`run` or `run_headless`). The callback lives for
the program's duration, since the ABI has no unsubscribe, so capture
accordingly.

## Requirements

- A C++17 compiler (tested with g++ and clang++).
- The Lumen C library, `liblumen`, built as a `cdylib` or `staticlib`.

Build the C library from the workspace root:

```sh
cargo build -p lumen            # debug   -> target/debug/liblumen.{so,a}
cargo build -p lumen --release  # release -> target/release/liblumen.{so,a}
```

The C ABI header lives at `include/lumen.h`, with the
cbindgen-generated `lumen_simple.h` beside it. Both are committed, so there is
no generation step to consume them. This SDK targets ABI 0.12. The
compatibility check, `abi_compatible()`, compares the cbindgen-generated
`LUMEN_ABI_VERSION` (the constant `lumen_abi_version()` itself returns)
against the loaded library, so it tracks the library exactly rather than the
hand-written mirror in `lumen.h`.

## Linking

### In-tree, via CMake (the `apps/sysmon` pattern)

```cmake
add_subdirectory(path/to/sdk/cpp lumen_cpp)   # provides lumen::cpp
add_executable(myapp main.cpp)
target_link_libraries(myapp PRIVATE
    lumen::cpp                                # headers (lumen.hpp + lumen.h)
    ${CARGO_TARGET_DIR}/debug/liblumen.so     # the built library
    pthread)
```

`lumen::cpp` is an `INTERFACE` target: it contributes the include paths for
both `<lumen.hpp>` and `<lumen.h>` and requires C++17.

### Installed, via pkg-config

```sh
c++ -std=c++17 main.cpp $(pkg-config --cflags --libs lumen) -o myapp
```

When pkg-config resolves `lumen`, the CMake target picks the link flags up
automatically, so `target_link_libraries(myapp PRIVATE lumen::cpp)` alone
suffices.

### Installed, via find_package

```cmake
find_package(lumen-cpp CONFIG REQUIRED)
target_link_libraries(myapp PRIVATE lumen::cpp)
```

### Loader path

`liblumen.so` must be findable at runtime: install it to a system
library directory, set `LD_LIBRARY_PATH`, or bake an `RPATH` pointing at
the cargo target directory (sysmon's CMake does the last).

## Error model

Exceptions are the primary error channel. The lifecycle failures the API
reports (a missing app directory, an ABI mismatch, a rejected `expose`) are
rare and pair naturally with RAII. Each throwing site turns a non-OK
`LumenStatus` into a `lumen::Error` carrying the status code (`.status()`) and
the thread-local `lumen_last_error()` text.

The thread-hot signal surface does not throw: `Signal<T>` get and set are
`noexcept`, and the raw getters return `std::optional<T>`. `App::run()`
returns the raw status and is `noexcept`; `run_checked()` and `run_headless()`
throw on a non-OK status.

No C++ exception crosses back into the C/Rust boundary. The callback and watch
trampolines catch everything and degrade, because unwinding through the Rust
FFI frames is undefined behavior.

## Callback & watch lifetimes

- `App::expose` / `App::on_click` / `App::on_close` move your callable
  onto the heap; the `App` owns it and stays valid for the whole run (`run`
  blocks and the `App` must outlive it). Anything captured by reference
  must outlive the `App`, so prefer locals declared before it, or globals.
  `on_close` (ABI 0.5) fires on an OS close request before teardown and may
  veto: return `true` from a `bool()` handler to allow the close, `false` to
  keep the window open (a `void()` handler always allows). It does not fire
  under `run_headless`.
- `Signal<T>::watch` callables live for the program's duration, since the ABI
  has no unsubscribe. They are heap-anchored independently of the originating
  handle.
- Callbacks and watches fire on the Lumen tick thread, generally not the
  thread that built the `App`. Guard any shared mutable state they touch.
- `lumen::Signal<T>` and `lumen::raw::*` calls are safe from any thread.

## API surface

| Area | C++ | Wraps |
| --- | --- | --- |
| ABI | `abi_compatible()`, `header_abi_version()`, `runtime_abi_version()` | `LUMEN_API_VERSION`, `lumen_abi_version` |
| Lifecycle | `lumen::App` - ctor `Options`, `title`, `size`, `expose`, `on_click`, `on_close`, `run`, `run_checked`, `run_headless` | `lumen_app_new/set_title/set_size/expose_v2/on_click/on_close/run/run_headless/free` |
| Signals | `lumen::Signal<T>` - ctor, `get`/`set`, `operator= * += -=`, `watch` | `lumen_signal_set_*64/bool/color`, getters, `lumen_signal_watch` |
| Dynamic DOM | `lumen::dom` - `spawn`, `get_by_id`, `query`, `Node`, `Event`, `Listener` | `lumen_node_*`, `lumen_dom_*` |
| Colors | `lumen::Color` - `from_hex`, `to_hex` | RGBA bytes |
| Navigation | `lumen::navigate`, `navigate_back`, `navigate_forward`, `current_page` | `lumen_navigate*`, `lumen_current_page` |
| Values | `lumen::Value` (nil/bool/int/float/string/array/map), `lumen::Args` | `LumenValue`, `LumenFn` |
| Errors | `lumen::Error`, `last_error()`, `status_message()` | `lumen_last_error`, `lumen_status_message` |

## Appendix - the raw layer (`lumen::raw`)

`namespace lumen::raw` is the thin, stringly surface `Signal<T>` is built
on: each free function is exactly one `lumen_signal_*` C call. Reach for it
only when you need the raw ABI, notably `set_array` (array signals for
`<for>` markup, which are not scalars), or the stringly text setters:

```cpp
lumen::raw::set("greeting", "hello");                 // stringly text signal
lumen::raw::set_int("count", 5);                      // typed scalar
lumen::raw::set_array("rows", lumen::Value::array({   // <for> array signal
    lumen::Value::map({{"id", lumen::Value::string("a")}}),
}));
std::optional<std::string> s = lumen::raw::get_string("greeting");
```

Read-back caveat: `get_string` / `array_len` / `array_field` return the
value the *embedder* last pushed through the FFI, not live in-app state
(a script `signals.x.set(..)` or a two-way input binding is not visible
here). The typed `Signal<T>` layer hides all of this.
