// Lumen C++17 SDK - header-only RAII wrapper over the Lumen C ABI.
//
// This is the canonical C++ binding for the Lumen UI framework. It wraps
// the C ABI declared in <lumen.h> (opaque `LumenApp`, tagged `LumenValue`,
// signal mutators, typed scalar accessors, ABI-version probe) in modern,
// RAII-safe C++.
//
// What you get:
//   - `lumen::App`     RAII handle for the app lifecycle. The constructor
//                      verifies the loaded library's ABI version against
//                      the header this SDK was compiled with and throws on
//                      an incompatible mismatch. Builder-style config.
//   - `lumen::Value`   Owned, recursive value type (nil/bool/int/float/
//                      string/array/map) with materialisation into the
//                      borrowed C `LumenValue` tree for one ABI call.
//   - `lumen::Signal`  Thread-safe signal surface - typed scalar get/set
//                      accessors (string, int64, float64, bool, RGBA
//                      color) that `bind-text` markup reads, plus the
//                      record-shaped array signals `<for>` consumes.
//   - `std::function`  callbacks exposed to the script runtime, bridged
//                      through the C `LumenFn` trampoline.
//
// Error model
//   Exceptions are the primary error channel. C++17 has no std::expected,
//   and the lifecycle failures this SDK reports (bad app directory, ABI
//   mismatch, a rejected `expose`) are genuinely exceptional - pairing
//   them with RAII cleanup is the idiomatic fit. Every throwing site maps
//   a non-OK `LumenStatus` into a `lumen::Error` carrying both the status
//   code and the thread-local `lumen_last_error()` text.
//
//   The thread-hot signal surface deliberately does NOT throw: setters
//   return `LumenStatus` (noexcept) and typed getters return
//   `std::optional<T>` (noexcept). A background sampler thread pushing
//   values every few milliseconds should never pay for - or have to
//   propagate - an exception, and "the signal was never set" is an
//   ordinary absent-value case, not an error. See README.md.
//
//   No C++ exception ever crosses the C boundary: the callback trampoline
//   wraps the user's `std::function` in `catch (...)` and degrades to a
//   nil return, because unwinding through the Rust FFI frames is UB.
//
// Callback lifetimes (READ THIS)
//   `App::expose` moves your callable onto the heap and hands the C side a
//   raw pointer to it as `user_data`. That heap object is owned by the
//   `App` and stays alive until the `App` is destroyed. Because
//   `App::run()` blocks until the window closes and the `App` must remain
//   alive across that call, your callbacks are valid for the whole run.
//   Any state your callback captures by reference must outlive the `App`.
//
// Threading
//   Every `lumen::Signal` call is safe from any thread (the C ABI routes
//   through a thread-safe channel into the Lumen ECS world). Exposed
//   callbacks fire on the Lumen script thread, which is generally NOT the
//   thread that constructed the `App`.
//
// Dependencies: this header + <lumen.h> + the C++17 standard library.
// Nothing else.

#ifndef LUMEN_SDK_HPP
#define LUMEN_SDK_HPP

#pragma once

#include <lumen.h>

#include <array>
#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <type_traits>
#include <utility>
#include <vector>

namespace lumen {

// =====================================================================
// ABI version
// =====================================================================

/// Packed ABI version this SDK header was compiled against.
///
/// Prefers `LUMEN_ABI_VERSION` - the cbindgen-generated macro in
/// <lumen_simple.h>, mechanically derived from the Rust source of truth
/// (`lumen_abi_version()` returns the very same constant), so it can never
/// drift from the library. Falls back to the hand-written `LUMEN_API_VERSION`
/// mirror in <lumen.h> only if the generated header is absent. (The mirror is
/// drift-prone: it has trailed the generated value across additive ABI bumps,
/// which would make every `App` construction spuriously fail the compatibility
/// check below - so the generated constant is the correct thing to compare.)
constexpr std::uint32_t header_abi_version() noexcept {
#ifdef LUMEN_ABI_VERSION
    return LUMEN_ABI_VERSION;
#else
    return LUMEN_API_VERSION;
#endif
}

/// Packed ABI version reported by the loaded Lumen library.
inline std::uint32_t runtime_abi_version() noexcept { return lumen_abi_version(); }

/// True when the loaded library is ABI-compatible with this header.
/// Major+minor must match exactly (the top 24 bits); patch is ignored.
/// Additive changes bump minor, so a differing minor is treated as
/// incompatible on the conservative side.
inline bool abi_compatible() noexcept {
    return (header_abi_version() >> 8) == (runtime_abi_version() >> 8);
}

// =====================================================================
// Error
// =====================================================================

/// Thrown by the lifecycle API on a non-OK `LumenStatus`. Carries the
/// status code plus the thread-local error text captured at throw time.
class Error : public std::runtime_error {
public:
    explicit Error(std::string msg, LumenStatus status = LUMEN_ERR_RUNTIME)
        : std::runtime_error(std::move(msg)), status_(status) {}

    /// The originating C status code.
    LumenStatus status() const noexcept { return status_; }

private:
    LumenStatus status_;
};

/// Thread-local error text from the last failing `lumen_*` call, or "".
inline std::string last_error() {
    const char* p = lumen_last_error();
    return p ? std::string(p) : std::string();
}

/// Static, canonical description of a status code (no `last_error`
/// round-trip). Never null; do not free.
inline std::string status_message(LumenStatus s) {
    const char* p = lumen_status_message(s);
    return p ? std::string(p) : std::string();
}

namespace detail {

/// Throw a `lumen::Error` when `s` is not `LUMEN_OK`. `op` names the
/// failing operation; the message folds in the canonical status name and
/// the thread-local error text when present.
inline void check(LumenStatus s, const char* op) {
    if (s == LUMEN_OK) return;
    std::string msg = std::string(op) + ": " + status_message(s);
    std::string detail = last_error();
    if (!detail.empty()) { msg += " ("; msg += detail; msg += ")"; }
    throw Error(std::move(msg), s);
}

} // namespace detail

// =====================================================================
// Color - RGBA bytes, the color-typed signal value.
// =====================================================================

/// An RGBA color, four bytes each in 0..=255. Designated-init friendly:
/// `lumen::Color{.r = 255, .g = 128, .b = 0}` or `lumen::Color{255,128,0}`.
struct Color {
    std::uint8_t r = 0;
    std::uint8_t g = 0;
    std::uint8_t b = 0;
    std::uint8_t a = 255;

    friend bool operator==(const Color& x, const Color& y) noexcept {
        return x.r == y.r && x.g == y.g && x.b == y.b && x.a == y.a;
    }
    friend bool operator!=(const Color& x, const Color& y) noexcept { return !(x == y); }

    /// Parse a CSS-style hex color: `#rgb`, `#rgba`, `#rrggbb`, or
    /// `#rrggbbaa` (the leading `#` is optional; short forms expand per CSS,
    /// so `#f80` -> `#ff8800`, and a 6-digit value defaults to opaque alpha).
    /// Matches the Python SDK's `Color.from_hex`. Throws `lumen::Error`
    /// (`LUMEN_ERR_BAD_ARG`) on a malformed string.
    ///
    /// Offered as a factory rather than a `Color(std::string_view)`
    /// constructor on purpose: adding any user-declared constructor would
    /// make `Color` a non-aggregate and forfeit both designated-init
    /// (`Color{.r = 255, .g = 128}`) and braced-init (`Color{255, 128, 0}`),
    /// which are the primary construction forms. A static member function
    /// keeps the aggregate intact.
    ///
    ///     auto orange = lumen::Color::from_hex("#ff8000");
    static Color from_hex(std::string_view hex) {
        std::string s(hex);
        if (!s.empty() && s.front() == '#') s.erase(0, 1);
        if (s.size() == 3 || s.size() == 4) { // expand each nibble: f80 -> ff8800
            std::string e;
            e.reserve(s.size() * 2);
            for (char c : s) { e.push_back(c); e.push_back(c); }
            s = std::move(e);
        }
        if (s.size() == 6) s += "ff"; // default opaque alpha
        auto bad = [&](const char* why) {
            return Error("Color::from_hex: '" + std::string(hex) + "' " + why,
                         LUMEN_ERR_BAD_ARG);
        };
        if (s.size() != 8) throw bad("is not #rgb / #rgba / #rrggbb / #rrggbbaa");
        auto nibble = [](char c) -> int {
            if (c >= '0' && c <= '9') return c - '0';
            if (c >= 'a' && c <= 'f') return c - 'a' + 10;
            if (c >= 'A' && c <= 'F') return c - 'A' + 10;
            return -1;
        };
        std::uint8_t out[4];
        for (int i = 0; i < 4; ++i) {
            int hi = nibble(s[i * 2]), lo = nibble(s[i * 2 + 1]);
            if (hi < 0 || lo < 0) throw bad("has non-hex digits");
            out[i] = static_cast<std::uint8_t>((hi << 4) | lo);
        }
        return Color{out[0], out[1], out[2], out[3]};
    }

    /// Render as `#rrggbbaa` (lower-case, always eight digits). Round-trips
    /// with `from_hex`.
    std::string to_hex() const {
        static constexpr char digits[] = "0123456789abcdef";
        const std::uint8_t ch[4] = {r, g, b, a};
        std::string out = "#00000000";
        for (int i = 0; i < 4; ++i) {
            out[1 + i * 2]     = digits[(ch[i] >> 4) & 0xF];
            out[1 + i * 2 + 1] = digits[ch[i] & 0xF];
        }
        return out;
    }
};

// =====================================================================
// Value - owned, recursive value tree
//
// A `Value` owns its payload (strings, child arrays, map entries). Its
// `view()` method materialises a borrowed C `LumenValue` tree whose
// pointers reference `*this` (and caller-supplied arenas); that view is
// valid only while the `Value` is alive, unmoved, and the arenas are in
// scope. Callers pass the view across the ABI within a single call - the
// Rust side copies immediately, so the borrow only needs to survive that
// call.
// =====================================================================

class Value {
public:
    Value() noexcept = default;

    static Value nil() { return Value{}; }
    static Value boolean(bool v)     { Value r; r.kind_ = LUMEN_BOOL;   r.bool_ = v;  return r; }
    static Value integer(std::int64_t v) { Value r; r.kind_ = LUMEN_INT; r.int_ = v; return r; }
    static Value floating(double v)  { Value r; r.kind_ = LUMEN_FLOAT;  r.float_ = v; return r; }
    static Value string(std::string v) { Value r; r.kind_ = LUMEN_STRING; r.str_ = std::move(v); return r; }
    static Value array(std::vector<Value> v) { Value r; r.kind_ = LUMEN_ARRAY; r.array_ = std::move(v); return r; }

    static Value map(std::vector<std::pair<std::string, Value>> v) {
        Value r; r.kind_ = LUMEN_MAP; r.map_ = std::move(v); return r;
    }
    static Value map(std::initializer_list<std::pair<std::string, Value>> il) {
        Value r; r.kind_ = LUMEN_MAP;
        r.map_.reserve(il.size());
        for (auto const& p : il) r.map_.push_back(p);
        return r;
    }

    LumenKind kind() const noexcept { return kind_; }

    bool               as_bool()   const { return bool_; }
    std::int64_t       as_int()    const { return int_; }
    double             as_float()  const { return float_; }
    const std::string& as_string() const { return str_; }
    const std::vector<Value>& as_array() const { return array_; }
    const std::vector<std::pair<std::string, Value>>& as_map() const { return map_; }

    /// Materialise a borrowed C `LumenValue` tree. `view_arena` /
    /// `entry_arena` keep the child container storage alive; they must
    /// outlive every use of the returned `LumenValue`.
    LumenValue view(std::vector<std::vector<LumenValue>>& view_arena,
                    std::vector<std::vector<LumenMapEntry>>& entry_arena) const {
        LumenValue out{};
        out.kind = kind_;
        switch (kind_) {
            case LUMEN_NIL:    out.as_.integer = 0; break;
            case LUMEN_BOOL:   out.as_.boolean = bool_ ? 1 : 0; break;
            case LUMEN_INT:    out.as_.integer = int_; break;
            case LUMEN_FLOAT:  out.as_.float_  = float_; break;
            case LUMEN_STRING: out.as_.string  = str_.c_str(); break;
            case LUMEN_ARRAY: {
                std::vector<LumenValue> items;
                items.reserve(array_.size());
                for (auto const& v : array_) items.push_back(v.view(view_arena, entry_arena));
                view_arena.push_back(std::move(items));
                auto const& back = view_arena.back();
                out.as_.array.items = back.data();
                out.as_.array.len   = back.size();
                break;
            }
            case LUMEN_MAP: {
                std::vector<LumenMapEntry> entries;
                entries.reserve(map_.size());
                for (auto const& [k, v] : map_) {
                    LumenMapEntry e{};
                    e.key   = k.c_str();
                    e.value = v.view(view_arena, entry_arena);
                    entries.push_back(e);
                }
                entry_arena.push_back(std::move(entries));
                auto const& back = entry_arena.back();
                out.as_.map.entries = back.data();
                out.as_.map.len     = back.size();
                break;
            }
        }
        return out;
    }

    /// Deep-copy a borrowed C `LumenValue` (e.g. an argv slot) into an
    /// owned `Value` tree.
    static Value adopt(const LumenValue& src) {
        switch (src.kind) {
            case LUMEN_NIL:    return Value::nil();
            case LUMEN_BOOL:   return Value::boolean(src.as_.boolean != 0);
            case LUMEN_INT:    return Value::integer(src.as_.integer);
            case LUMEN_FLOAT:  return Value::floating(src.as_.float_);
            case LUMEN_STRING: return Value::string(src.as_.string ? std::string(src.as_.string)
                                                                   : std::string());
            case LUMEN_ARRAY: {
                std::vector<Value> out;
                out.reserve(src.as_.array.len);
                for (std::size_t i = 0; i < src.as_.array.len; ++i)
                    out.push_back(Value::adopt(src.as_.array.items[i]));
                return Value::array(std::move(out));
            }
            case LUMEN_MAP: {
                std::vector<std::pair<std::string, Value>> out;
                out.reserve(src.as_.map.len);
                for (std::size_t i = 0; i < src.as_.map.len; ++i) {
                    const LumenMapEntry& e = src.as_.map.entries[i];
                    out.emplace_back(e.key ? std::string(e.key) : std::string(),
                                     Value::adopt(e.value));
                }
                return Value::map(std::move(out));
            }
        }
        return Value::nil();
    }

private:
    LumenKind    kind_  = LUMEN_NIL;
    bool         bool_  = false;
    std::int64_t int_   = 0;
    double       float_ = 0.0;
    std::string  str_;
    std::vector<Value> array_;
    std::vector<std::pair<std::string, Value>> map_;
};

// =====================================================================
// Args - read-only view of a callback's (argc, argv).
// =====================================================================

class Args {
public:
    Args(int argc, const LumenValue* argv) noexcept : argc_(argc), argv_(argv) {}

    int  size()  const noexcept { return argc_; }
    bool empty() const noexcept { return argc_ == 0; }

    /// Raw borrowed slot. Valid only for the duration of the callback.
    const LumenValue& raw(int i) const noexcept { return argv_[i]; }

    /// Deep-copy the i-th argument into an owned `Value`.
    Value at(int i) const { return Value::adopt(argv_[i]); }

private:
    int argc_;
    const LumenValue* argv_;
};

// =====================================================================
// App - RAII handle around LumenApp*.
// =====================================================================

class App {
public:
    /// Callback exposed to the script runtime. Captures are fine; the
    /// callable is heap-owned by the `App` (see the callback-lifetime
    /// note at the top of this header).
    using Callback = std::function<Value(Args)>;

    /// Id-scoped native click handler. Receives the clicked element id.
    using ClickCallback = std::function<void(std::string)>;

    /// Void click handler for the ergonomic `on_click(id, []{ ... })`
    /// overload that doesn't care which id fired.
    using VoidCallback = std::function<void()>;

    /// App-level close handler (ABI 0.5). Returns `true` to allow the close
    /// or `false` to veto it and keep the window open. See `on_close`.
    using CloseCallback = std::function<bool()>;

    /// Designated-initializer config for the `App` constructor:
    /// `App app("dir", {.title = "Counter", .width = 800, .height = 600})`.
    /// Every field is optional - an unset `title` (nullptr) and a
    /// zero-sized window fall back to `lumen.toml` / the directory name.
    struct Options {
        /// Window title override (nullptr = use lumen.toml / dir name).
        const char* title = nullptr;
        /// Initial window width in logical pixels (0 = use lumen.toml).
        std::uint32_t width = 0;
        /// Initial window height in logical pixels (0 = use lumen.toml).
        std::uint32_t height = 0;
    };

    /// Construct an app rooted at `dir` (must contain lumen.toml /
    /// main.lmn), applying `opts`. Verifies ABI compatibility first, then
    /// allocates the handle. Throws `lumen::Error` on ABI mismatch or
    /// allocation failure. The `title()` / `size()` builder methods remain
    /// for fluent post-construction tweaks.
    ///
    ///     lumen::App app("counter_app", {.title = "Counter"});
    App(std::string_view dir, const Options& opts) {
        if (!abi_compatible()) {
            throw Error("lumen ABI mismatch: header " +
                            std::to_string(header_abi_version()) + " vs library " +
                            std::to_string(runtime_abi_version()),
                        LUMEN_ERR_INTERNAL);
        }
        std::string z(dir);
        raw_ = lumen_app_new(z.c_str());
        if (!raw_) throw Error("lumen_app_new: " + last_error(), LUMEN_ERR_BAD_PATH);
        if (opts.title) title(opts.title);
        if (opts.width && opts.height) size(opts.width, opts.height);
    }

    /// Construct with default options (title / size from lumen.toml).
    /// A separate overload - not a `= {}` default argument - because GCC's
    /// C++17 designated-initializer extension refuses to synthesise a
    /// nested-aggregate default argument.
    explicit App(std::string_view dir) : App(dir, Options{}) {}

    App(const App&) = delete;
    App& operator=(const App&) = delete;

    App(App&& other) noexcept
        : raw_(other.raw_),
          callbacks_(std::move(other.callbacks_)),
          click_callbacks_(std::move(other.click_callbacks_)),
          close_callback_(std::move(other.close_callback_)) {
        other.raw_ = nullptr;
    }
    App& operator=(App&& other) noexcept {
        if (this != &other) {
            release();
            raw_ = other.raw_;
            callbacks_ = std::move(other.callbacks_);
            click_callbacks_ = std::move(other.click_callbacks_);
            close_callback_ = std::move(other.close_callback_);
            other.raw_ = nullptr;
        }
        return *this;
    }

    ~App() { release(); }

    /// Override the window title. Returns `*this` for chaining.
    App& title(std::string_view t) {
        std::string z(t);
        detail::check(lumen_app_set_title(raw_, z.c_str()), "lumen_app_set_title");
        return *this;
    }

    /// Override the initial window size in logical pixels.
    App& size(std::uint32_t w, std::uint32_t h) {
        detail::check(lumen_app_set_size(raw_, w, h), "lumen_app_set_size");
        return *this;
    }

    /// Expose `cb` to the script runtime under `name` with `arg_count`
    /// arity. The callable is moved to the heap and kept alive by this
    /// `App`; the script side calls it through a C trampoline.
    App& expose(std::string_view name, std::uint32_t arg_count, Callback cb) {
        auto owned = std::make_unique<Callback>(std::move(cb));
        void* ud = owned.get();
        callbacks_.push_back(std::move(owned));
        std::string z(name);
        detail::check(lumen_app_expose_v2(raw_, z.c_str(), arg_count, &App::trampoline, ud),
                      "lumen_app_expose_v2");
        return *this;
    }

    /// Convenience overload for `Value()` / `Value(Args)` callables
    /// (defaults to arity 0).
    template <typename F>
    App& expose(std::string_view name, F&& f) {
        return expose(name, /*arg_count=*/0, [fn = std::forward<F>(f)](Args args) -> Value {
            if constexpr (std::is_invocable_r_v<Value, F, Args>) {
                return fn(args);
            } else if constexpr (std::is_invocable_r_v<Value, F>) {
                (void)args;
                return fn();
            } else {
                static_assert(sizeof(F) == 0,
                              "expose(name, fn): fn must be callable as Value() or Value(Args).");
            }
        });
    }

    /// Register an id-scoped native click handler (ABI 0.3). `handler`
    /// fires once per click on the element whose id equals `id`, routed by
    /// the runtime - no `main.lmn` forwarding boilerplate and no per-SDK
    /// dispatch table over the global `on_click(id)` hook. A second
    /// registration for the same id replaces the first. The callable is
    /// heap-owned by this `App` (same lifetime rules as `expose`).
    ///
    /// Accepts either a `void()` handler (the common case - you already
    /// know the id) or a `void(std::string)` handler that receives the
    /// clicked element id:
    ///
    ///     app.on_click("bump", [&] { count += 1; });
    ///     app.on_click("row",  [&](std::string id) { select(id); });
    template <typename F>
    App& on_click(std::string_view id, F&& f) {
        if constexpr (std::is_invocable_v<F, std::string>) {
            return install_click(id, ClickCallback(std::forward<F>(f)));
        } else if constexpr (std::is_invocable_v<F>) {
            return install_click(
                id, ClickCallback([fn = std::forward<F>(f)](std::string) { fn(); }));
        } else {
            static_assert(sizeof(F) == 0,
                          "on_click(id, fn): fn must be callable as void() or void(std::string).");
        }
    }

    /// Register an app-level close hook (ABI 0.5). It fires once per OS
    /// close request - the window close button, or (Unix) the first
    /// SIGINT/SIGTERM - BEFORE the runtime tears down the window, GPU
    /// state, or script host, and can veto the close.
    ///
    /// Accepts either a `bool()` handler (return `true` to allow the close,
    /// `false` to keep the window open) or a `void()` handler (a shutdown
    /// notification that always allows the close):
    ///
    ///     app.on_close([&] { save(); return true; });  // save, then close
    ///     app.on_close([&] { return !dirty; });         // veto while unsaved
    ///     app.on_close([&] { flush_logs(); });          // void: always close
    ///
    /// A second registration replaces the first. The callable is heap-owned
    /// by this `App` (same lifetime rules as `expose` / `on_click`). It does
    /// NOT fire under `run_headless` (there is no OS close request). On Unix a
    /// second SIGINT/SIGTERM bypasses the hook, so a vetoing handler cannot
    /// wedge shutdown. If the handler throws, the close is allowed (the
    /// exception is swallowed - unwinding across the C boundary is UB).
    template <typename F>
    App& on_close(F&& f) {
        if constexpr (std::is_invocable_r_v<bool, F>) {
            return install_close(CloseCallback(std::forward<F>(f)));
        } else if constexpr (std::is_invocable_v<F>) {
            return install_close(
                CloseCallback([fn = std::forward<F>(f)]() -> bool { fn(); return true; }));
        } else {
            static_assert(sizeof(F) == 0,
                          "on_close(fn): fn must be callable as bool() or void().");
        }
    }

    /// Enter the Lumen event loop and block until the window closes.
    /// Returns the raw exit status (`LUMEN_OK` on clean shutdown). Consumes
    /// the underlying handle (a second `run*` on the same `App` returns
    /// `LUMEN_ERR_INVALID_HANDLE`), so it is safe to call on a named
    /// lvalue - `return app.run();` - which is the common shape given the
    /// `App` also owns the exposed/click callbacks that must outlive the
    /// run.
    LumenStatus run() noexcept {
        LumenApp* app = raw_;
        raw_ = nullptr;
        return lumen_app_run(app);
    }

    /// Like `run()`, but throws `lumen::Error` on a non-OK status.
    void run_checked() {
        LumenApp* app = raw_;
        raw_ = nullptr;
        detail::check(lumen_app_run(app), "lumen_app_run");
    }

    /// Drive `ticks` main-schedule ticks without opening a window or GPU
    /// surface (ABI 0.3) - the CI / no-display entry point. Consumes the
    /// handle. Signal round-trips, exposed callbacks, script execution,
    /// `<for>` / `<if>` reconciliation, and `Signal::watch` firing all run;
    /// native click handlers do not fire (no input is injected headless).
    /// `ticks == 0` builds-and-drops. Throws `lumen::Error` on a non-OK
    /// status.
    void run_headless(std::uint32_t ticks = 1) {
        LumenApp* app = raw_;
        raw_ = nullptr;
        detail::check(lumen_app_run_headless(app, ticks), "lumen_app_run_headless");
    }

private:
    void release() noexcept {
        if (raw_) { lumen_app_free(raw_); raw_ = nullptr; }
    }

    App& install_click(std::string_view id, ClickCallback cb) {
        auto owned = std::make_unique<ClickCallback>(std::move(cb));
        void* ud = owned.get();
        click_callbacks_.push_back(std::move(owned));
        std::string z(id);
        detail::check(lumen_app_on_click(raw_, z.c_str(), &App::click_trampoline, ud),
                      "lumen_app_on_click");
        return *this;
    }

    App& install_close(CloseCallback cb) {
        // A second registration REPLACES the first (matching the C ABI), so a
        // single owned slot suffices; resetting it also drops the prior
        // callable now that the runtime no longer points at it.
        close_callback_ = std::make_unique<CloseCallback>(std::move(cb));
        detail::check(
            lumen_app_on_close(raw_, &App::close_trampoline, close_callback_.get()),
            "lumen_app_on_close");
        return *this;
    }

    // C-callable bridge, ABI 0.3 out-parameter form (LumenFnV2): writes
    // the result through `out` instead of returning by value, so the
    // callback ABI carries no aggregate (`sret`) return. NEVER lets a
    // C++ exception escape into the Rust FFI frames (that would be UB);
    // degrades to a nil result instead.
    static void trampoline(LumenValue* out, int argc, const LumenValue* argv,
                           void* user) noexcept {
        try {
            auto* cb = reinterpret_cast<Callback*>(user);
            Value result = (*cb)(Args{argc, argv});
            // Keep the returned Value + its borrowed C view alive until
            // this function returns; Rust copies before we unwind. A
            // two-slot thread_local ping-pong guards the (currently
            // impossible) re-entrant call without a copy in between.
            thread_local Value owners[2];
            thread_local std::vector<std::vector<LumenValue>> view_arenas[2];
            thread_local std::vector<std::vector<LumenMapEntry>> entry_arenas[2];
            thread_local unsigned slot = 0;
            slot ^= 1u;
            owners[slot] = std::move(result);
            view_arenas[slot].clear();
            entry_arenas[slot].clear();
            *out = owners[slot].view(view_arenas[slot], entry_arenas[slot]);
        } catch (...) {
            out->kind = LUMEN_NIL;
            out->as_.integer = 0;
        }
    }

    // C-callable bridge for id-scoped native click handlers. Like
    // `trampoline`, never lets a C++ exception escape into the Rust FFI
    // frames.
    static void click_trampoline(const char* id, void* user) noexcept {
        try {
            auto* cb = reinterpret_cast<ClickCallback*>(user);
            (*cb)(id ? std::string(id) : std::string());
        } catch (...) {
            // Swallow - unwinding across the C boundary is UB.
        }
    }

    // C-callable bridge for the app-level close hook. Returns nonzero to
    // allow the close, 0 to veto (the LumenCloseFn contract). Never lets a
    // C++ exception escape into the Rust FFI frames; on an exception it
    // allows the close so a throwing handler can't wedge shutdown.
    static int close_trampoline(void* user) noexcept {
        try {
            auto* cb = reinterpret_cast<CloseCallback*>(user);
            return (*cb)() ? 1 : 0;
        } catch (...) {
            return 1;
        }
    }

    LumenApp* raw_ = nullptr;
    // unique_ptr keeps callback addresses stable across vector growth so
    // the raw user_data pointers handed to C stay valid.
    std::vector<std::unique_ptr<Callback>> callbacks_;
    std::vector<std::unique_ptr<ClickCallback>> click_callbacks_;
    // Single owned slot: on_close replaces rather than accumulates.
    std::unique_ptr<CloseCallback> close_callback_;
};

// =====================================================================
// raw - thin signal surface. Free functions; no App needed.
//
// This is the LOW-LEVEL layer beneath the typed `lumen::Signal<T>` handle
// below; reach for it only when you need a raw `lumen_signal_*` C call.
// Scalars are one typed family - `set` / `get_string`, `set_int` /
// `get_int`, and friends carry a typed `PropertyValue` cell with no
// stringify/parse round-trip, and `bind-text` markup reads any of them.
// Array signals (`set_array`, `array_len`, `array_field`) are a separate,
// record-shaped family that `<for>` markup consumes.
//
// Setters return `LumenStatus` and are `noexcept`; getters return
// `std::optional<T>` (empty when the signal holds no value of that
// type). See the error-model note at the top of this header.
// =====================================================================

namespace raw {

using Rgba = std::array<std::uint8_t, 4>;

// ---- Scalar signals (bind-text) ------------------------------------

/// Set a scalar signal to a UTF-8 string.
inline LumenStatus set(std::string_view name, std::string_view value) noexcept {
    std::string n(name), v(value);
    return lumen_signal_set_str(n.c_str(), v.c_str());
}
/// Set a scalar signal to a 64-bit integer.
inline LumenStatus set(std::string_view name, std::int64_t value) noexcept {
    std::string n(name);
    return lumen_signal_set_int64(n.c_str(), value);
}
/// Set a scalar signal to a double.
inline LumenStatus set(std::string_view name, double value) noexcept {
    std::string n(name);
    return lumen_signal_set_float64(n.c_str(), value);
}

/// Replace the rows of an array signal. `v` must be a `LUMEN_ARRAY` of
/// `LUMEN_MAP` rows (each map becomes one `<for>` row). Lumen copies
/// immediately; buffers may be freed as soon as this returns.
inline LumenStatus set_array(std::string_view name, const Value& v) noexcept {
    std::string n(name);
    std::vector<std::vector<LumenValue>> va;
    std::vector<std::vector<LumenMapEntry>> ea;
    LumenValue lv = v.view(va, ea);
    return lumen_signal_set_array(n.c_str(), &lv);
}

/// Clear a signal (scalar -> empty string, array -> empty).
inline LumenStatus clear(std::string_view name) noexcept {
    std::string n(name);
    return lumen_signal_clear(n.c_str());
}

// ---- String / array read-back --------------------------------------
//
// `get_string` and the array getters read what the embedder last pushed
// through the FFI, not live in-app state (a script `signals.x.set(..)`
// or a two-way input binding is not visible through them). Empty
// optional when the signal was never set through the FFI. The number,
// bool, and color getters below do see in-app writes.

namespace detail {

/// Two-call size-then-fill wrapper for the string-out ABI convention.
/// `call(buf, buf_len, out_len)` invokes the underlying getter with a
/// pre-bound name/row/field; returns the value or std::nullopt.
template <typename Call>
inline std::optional<std::string> read_string_out(Call&& call) noexcept {
    std::size_t needed = 0;
    LumenStatus s = call(nullptr, 0, &needed);
    if (s == LUMEN_OK) return std::string();
    if (s != LUMEN_ERR_BUFFER_TOO_SMALL) return std::nullopt;
    std::string out(needed ? needed - 1 : 0, '\0');
    s = call(out.data(), needed, &needed);
    if (s != LUMEN_OK) return std::nullopt;
    out.resize(needed);
    return out;
}

} // namespace detail

/// Read a scalar signal back as a string.
inline std::optional<std::string> get_string(std::string_view name) noexcept {
    std::string n(name);
    return detail::read_string_out([&](char* buf, std::size_t len, std::size_t* out) {
        return lumen_signal_get_str(n.c_str(), buf, len, out);
    });
}

/// Number of rows in an array signal set through `set_array`.
inline std::optional<std::size_t> array_len(std::string_view name) noexcept {
    std::string n(name);
    std::size_t out = 0;
    if (lumen_signal_array_len(n.c_str(), &out) == LUMEN_OK) return out;
    return std::nullopt;
}

/// Read one field of one row (`record[field]` as a string) of an array
/// signal set through `set_array`.
inline std::optional<std::string> array_field(std::string_view name, std::size_t row,
                                              std::string_view field) noexcept {
    std::string n(name), f(field);
    return detail::read_string_out([&](char* buf, std::size_t len, std::size_t* out) {
        return lumen_signal_array_get_field(n.c_str(), row, f.c_str(), buf, len, out);
    });
}

// ---- Typed scalar signals ------------------------------------------

inline LumenStatus set_int(std::string_view name, std::int64_t value) noexcept {
    std::string n(name);
    return lumen_signal_set_int64(n.c_str(), value);
}
inline std::optional<std::int64_t> get_int(std::string_view name) noexcept {
    std::string n(name);
    std::int64_t out = 0;
    if (lumen_signal_get_int64(n.c_str(), &out) == LUMEN_OK) return out;
    return std::nullopt;
}

inline LumenStatus set_float(std::string_view name, double value) noexcept {
    std::string n(name);
    return lumen_signal_set_float64(n.c_str(), value);
}
inline std::optional<double> get_float(std::string_view name) noexcept {
    std::string n(name);
    double out = 0.0;
    if (lumen_signal_get_float64(n.c_str(), &out) == LUMEN_OK) return out;
    return std::nullopt;
}

inline LumenStatus set_bool(std::string_view name, bool value) noexcept {
    std::string n(name);
    return lumen_signal_set_bool(n.c_str(), value);
}
inline std::optional<bool> get_bool(std::string_view name) noexcept {
    std::string n(name);
    bool out = false;
    if (lumen_signal_get_bool(n.c_str(), &out) == LUMEN_OK) return out;
    return std::nullopt;
}

/// Set an RGBA color signal (each channel 0..=255).
inline LumenStatus set_color(std::string_view name, Rgba rgba) noexcept {
    std::string n(name);
    return lumen_signal_set_color(n.c_str(), rgba.data());
}
inline std::optional<Rgba> get_color(std::string_view name) noexcept {
    std::string n(name);
    Rgba out{};
    if (lumen_signal_get_color(n.c_str(), out.data()) == LUMEN_OK) return out;
    return std::nullopt;
}

/// Subscribe to changes of the global signal `name` (ABI 0.4). See
/// `lumen::Signal<T>::watch` for the ergonomic typed wrapper; this raw
/// form hands you the `LumenValue` directly.
inline LumenStatus watch(std::string_view name, LumenWatchFn cb, void* user_data) noexcept {
    std::string n(name);
    return lumen_signal_watch(n.c_str(), cb, user_data);
}

} // namespace raw

// =====================================================================
// Navigation - file-based pages (ABI 0.6).
//
// Thin wrappers over the `lumen_navigate*` / `lumen_current_page` ABI:
// the same shared navigation bus the script `page("...")` builtin and the
// Rust SDK drive. A `path` is a page path (`"settings"`, `"/user/7"`,
// `"/"`), resolved by longest existing `.lmn` prefix - NOT a URL scheme.
// All four are thread-safe and `noexcept` (the read returns `std::nullopt`
// rather than throwing).
// =====================================================================

/// Navigate the active page to `path`. Equivalent to the script
/// `page("...")` command.
inline LumenStatus navigate(std::string_view path) noexcept {
    std::string p(path);
    return lumen_navigate(p.c_str());
}

/// Step one entry back in the in-memory history stack (no-op at the start).
inline LumenStatus navigate_back() noexcept { return lumen_navigate_back(); }

/// Step one entry forward in the in-memory history stack (no-op at the end).
inline LumenStatus navigate_forward() noexcept { return lumen_navigate_forward(); }

/// The current active page key, or `std::nullopt` before the first page
/// mounts. Reads the navigation current-page mirror (lags a resolved
/// navigation by at most one tick).
inline std::optional<std::string> current_page() noexcept {
    return raw::detail::read_string_out([&](char* buf, std::size_t len, std::size_t* out) {
        return lumen_current_page(buf, len, out);
    });
}

// =====================================================================
// Signal<T> - typed reactive handle over one named signal.
//
// The effortless scalar surface: construct with a name (+ optional
// initial), then read/write with operator* / operator= / .get() / .set()
// and the += / -= operators where they make sense, and subscribe with
// .watch(). Supported T: int64_t, double, bool, std::string, lumen::Color.
//
//     lumen::Signal<int64_t>     count{"count", 0};
//     lumen::Signal<std::string> label{"label", "0 clicks"};
//     count += 1;
//     label = std::to_string(*count) + " clicks";
//     count.watch([](int64_t n) { /* fires on the tick n commits */ });
//
// A watch is a real ABI subscription (lumen_signal_watch), fired on the
// Lumen tick thread when the value commits - not a polling loop. It only
// fires while an app is running (App::run / App::run_headless).
// =====================================================================

namespace detail {

/// Trait: is `T` a signal-representable scalar?
template <class T>
inline constexpr bool is_signal_scalar_v =
    std::is_same_v<T, std::int64_t> || std::is_same_v<T, double> ||
    std::is_same_v<T, bool> || std::is_same_v<T, std::string> || std::is_same_v<T, Color>;

/// Process-lifetime anchor for watch callbacks. The ABI has no
/// unsubscribe, so a registered `std::function` lives for the program's
/// duration; heap-owning it here keeps the `user_data` pointer stable
/// regardless of what happens to the originating `Signal` handle. The
/// `inline` function's `static` local is one instance across all TUs.
inline std::vector<std::shared_ptr<void>>& watch_anchors() {
    static std::vector<std::shared_ptr<void>> v;
    return v;
}

} // namespace detail

template <class T>
class Signal {
    static_assert(detail::is_signal_scalar_v<T>,
                  "lumen::Signal<T> supports only int64_t, double, bool, "
                  "std::string, and lumen::Color.");

public:
    /// Handle a signal named `name` without seeding it.
    explicit Signal(std::string name) : name_(std::move(name)) {}

    /// Handle a signal named `name` and push `initial` immediately.
    Signal(std::string name, T initial) : name_(std::move(name)) { set(std::move(initial)); }

    /// The signal name markup binds against.
    const std::string& name() const noexcept { return name_; }

    /// Read the current value, typed. Absent signals read as `T{}`.
    T get() const noexcept {
        if constexpr (std::is_same_v<T, std::int64_t>) {
            return raw::get_int(name_).value_or(0);
        } else if constexpr (std::is_same_v<T, double>) {
            return raw::get_float(name_).value_or(0.0);
        } else if constexpr (std::is_same_v<T, bool>) {
            return raw::get_bool(name_).value_or(false);
        } else if constexpr (std::is_same_v<T, std::string>) {
            return raw::get_string(name_).value_or(std::string());
        } else { // Color
            raw::Rgba c = raw::get_color(name_).value_or(raw::Rgba{});
            return Color{c[0], c[1], c[2], c[3]};
        }
    }

    /// Write `value` to the runtime, typed.
    void set(const T& value) noexcept {
        if constexpr (std::is_same_v<T, std::int64_t>) {
            raw::set_int(name_, value);
        } else if constexpr (std::is_same_v<T, double>) {
            raw::set_float(name_, value);
        } else if constexpr (std::is_same_v<T, bool>) {
            raw::set_bool(name_, value);
        } else if constexpr (std::is_same_v<T, std::string>) {
            raw::set(name_, value);
        } else { // Color
            raw::set_color(name_, raw::Rgba{value.r, value.g, value.b, value.a});
        }
    }

    /// `signal = value` - sugar over `set`.
    Signal& operator=(const T& value) noexcept {
        set(value);
        return *this;
    }

    /// `*signal` - sugar over `get`.
    T operator*() const noexcept { return get(); }

    /// `signal += delta` - for numeric and string signals.
    template <class U>
    Signal& operator+=(const U& delta) noexcept {
        set(get() + delta);
        return *this;
    }

    /// `signal -= delta` - for numeric signals.
    template <class U>
    Signal& operator-=(const U& delta) noexcept {
        set(get() - delta);
        return *this;
    }

    /// Call `fn(new_value)` every time this signal's value commits, on the
    /// Lumen tick thread (see the class note). Registers a real ABI
    /// subscription; the callback lives for the program's duration.
    void watch(std::function<void(T)> fn) {
        auto held = std::make_shared<std::function<void(T)>>(std::move(fn));
        detail::watch_anchors().push_back(held);
        detail::check(raw::watch(name_, &Signal::watch_trampoline, held.get()),
                      "lumen_signal_watch");
    }

private:
    static void watch_trampoline(const char* /*name*/, const LumenValue* value,
                                 void* user) noexcept {
        try {
            auto* fn = reinterpret_cast<std::function<void(T)>*>(user);
            (*fn)(from_watch_value(value));
        } catch (...) {
            // Never unwind across the C boundary.
        }
    }

    /// Decode the borrowed `LumenValue` the watch ABI delivers into `T`.
    /// Color arrives as a LUMEN_INT packed big-endian 0xRRGGBBAA.
    static T from_watch_value(const LumenValue* v) {
        if constexpr (std::is_same_v<T, std::int64_t>) {
            return v ? v->as_.integer : 0;
        } else if constexpr (std::is_same_v<T, double>) {
            if (!v) return 0.0;
            return v->kind == LUMEN_INT ? static_cast<double>(v->as_.integer) : v->as_.float_;
        } else if constexpr (std::is_same_v<T, bool>) {
            return v && (v->kind == LUMEN_BOOL ? v->as_.boolean != 0 : v->as_.integer != 0);
        } else if constexpr (std::is_same_v<T, std::string>) {
            return (v && v->kind == LUMEN_STRING && v->as_.string) ? std::string(v->as_.string)
                                                                   : std::string();
        } else { // Color
            std::int64_t p = v ? v->as_.integer : 0;
            return Color{static_cast<std::uint8_t>((p >> 24) & 0xFF),
                         static_cast<std::uint8_t>((p >> 16) & 0xFF),
                         static_cast<std::uint8_t>((p >> 8) & 0xFF),
                         static_cast<std::uint8_t>(p & 0xFF)};
        }
    }

    std::string name_;
};

// =====================================================================
// dom - dynamic DOM handles over the C-ABI DOM surface (design 4.1-4.8).
//
// A `dom::Node` is a thin RAII-free wrapper over a live element in the
// running app: a packed handle (index + generation) that marshals as one
// `LumenNode`. It mirrors the Rust SDK's `lumen::dom::Node` and the
// host-neutral surface every script host binds -- query and traverse the
// tree, read and write attributes / classes / text / inline style, build
// and rearrange nodes, inspect post-layout geometry and computed style,
// and bind event handlers.
//
// The layer stays thin: every method calls exactly one (or, for the
// single-key `attr` / `style` convenience readers, one map-returning) C
// export and owns none of the runtime's logic. Reads are soft -- a stale
// handle reads back `std::nullopt` / `false` / an empty container rather
// than throwing, matching the "stale handle is an ordinary absent value"
// contract of the signal getters above. Mutations are fire-and-forget:
// each queues on the command bus the app drains once per tick, so a
// `spawn` plus its chained edits materialize together on the next tick;
// they return the node for chaining and do not throw on a queue error.
// =====================================================================

namespace dom {

class Node;
class Event;
class Listener;

/// A post-layout box, `getBoundingClientRect`-class. `x` / `y` are local
/// to the parent origin; `client_*` are window coordinates.
struct Rect {
    double x = 0, y = 0, width = 0, height = 0, client_x = 0, client_y = 0;
};

/// Scroll offsets and their travel limits for a scroll container.
struct Scroll {
    double x = 0, y = 0, max_x = 0, max_y = 0;
};

namespace detail {

/// Adopt an owned C string (a `char**`-out getter result) into a
/// `std::string`, releasing it with `lumen_string_free`. Empty on null.
inline std::string take_owned_string(char* p) {
    if (!p) return std::string();
    std::string s(p);
    lumen_string_free(p);
    return s;
}

/// Read a `LumenKVList`-returning getter into a vector of pairs, freeing
/// the C buffer. Empty on a non-OK status (a stale handle reads empty).
template <typename Call>
inline std::vector<std::pair<std::string, std::string>> read_kvlist(Call&& call) {
    LumenKVList list{};
    std::vector<std::pair<std::string, std::string>> out;
    if (call(&list) != LUMEN_OK) return out;
    out.reserve(list.len);
    for (std::size_t i = 0; i < list.len; ++i) {
        const LumenKV& kv = list.ptr[i];
        out.emplace_back(kv.key ? std::string(kv.key) : std::string(),
                         kv.value ? std::string(kv.value) : std::string());
    }
    lumen_kvlist_free(list);
    return out;
}

/// Read a `LumenStrList`-returning getter into a vector, freeing the C
/// buffer. Empty on a non-OK status.
template <typename Call>
inline std::vector<std::string> read_strlist(Call&& call) {
    LumenStrList list{};
    std::vector<std::string> out;
    if (call(&list) != LUMEN_OK) return out;
    out.reserve(list.len);
    for (std::size_t i = 0; i < list.len; ++i)
        out.emplace_back(list.ptr[i] ? std::string(list.ptr[i]) : std::string());
    lumen_strlist_free(list);
    return out;
}

/// Read a `LumenNodeList`-returning getter into a vector of nodes, freeing
/// the C buffer. Defined out of line below (needs `Node` complete).
inline std::vector<Node> read_nodelist(LumenNodeList list);

/// Look one key out of a `(key, value)` pair vector.
inline std::optional<std::string> lookup(
    const std::vector<std::pair<std::string, std::string>>& kv, std::string_view key) {
    for (auto const& [k, v] : kv)
        if (k == key) return v;
    return std::nullopt;
}

} // namespace detail

/// A live element handle. Copy-cheap; addresses one node by packed handle.
/// A default-constructed `Node` is the invalid handle (`0`).
class Node {
public:
    Node() noexcept = default;
    explicit Node(LumenNode handle) noexcept : handle_(handle) {}

    /// The raw packed handle (index + generation), for FFI round-trip.
    LumenNode handle() const noexcept { return handle_; }

    friend bool operator==(Node a, Node b) noexcept { return a.handle_ == b.handle_; }
    friend bool operator!=(Node a, Node b) noexcept { return a.handle_ != b.handle_; }

    /// Whether this handle still names a live node.
    bool valid() const noexcept {
        int out = 0;
        return lumen_node_valid(handle_, &out) == LUMEN_OK && out != 0;
    }

    // -- traversal (design 4.1, 4.2) ------------------------------------

    /// The parent element.
    std::optional<Node> parent() const { return out_node(lumen_node_parent); }
    /// The first child.
    std::optional<Node> first_child() const { return out_node(lumen_node_first_child); }
    /// The last child.
    std::optional<Node> last_child() const { return out_node(lumen_node_last_child); }
    /// The next sibling.
    std::optional<Node> next() const { return out_node(lumen_node_next); }
    /// The previous sibling.
    std::optional<Node> prev() const { return out_node(lumen_node_prev); }

    /// The ordered child elements.
    std::vector<Node> children() const {
        LumenNodeList list{};
        if (lumen_node_children(handle_, &list) != LUMEN_OK) return {};
        return detail::read_nodelist(list);
    }

    /// The nearest ancestor-or-self matching `selector`.
    std::optional<Node> closest(std::string_view selector) const {
        std::string s(selector);
        LumenNode out = 0;
        if (lumen_node_closest(handle_, s.c_str(), &out) == LUMEN_OK && out != 0)
            return Node(out);
        return std::nullopt;
    }

    // -- attributes / class / text (design 4.4) -------------------------

    /// The full attribute map.
    std::vector<std::pair<std::string, std::string>> attrs() const {
        return detail::read_kvlist([&](LumenKVList* o) { return lumen_node_attrs(handle_, o); });
    }
    /// Read one attribute (convenience over `attrs()`).
    std::optional<std::string> attr(std::string_view name) const {
        return detail::lookup(attrs(), name);
    }
    /// Set an attribute (chainable). Known names route to typed components.
    Node set_attr(std::string_view name, std::string_view value) const {
        std::string n(name), v(value);
        lumen_node_set_attr(handle_, n.c_str(), v.c_str());
        return *this;
    }
    /// Remove an attribute (chainable).
    Node remove_attr(std::string_view name) const {
        std::string n(name);
        lumen_node_remove_attr(handle_, n.c_str());
        return *this;
    }

    /// Set the text content (chainable).
    Node set_text(std::string_view text) const {
        std::string t(text);
        lumen_node_set_text(handle_, t.c_str());
        return *this;
    }

    /// The full class list.
    std::vector<std::string> classes() const {
        return detail::read_strlist([&](LumenStrList* o) { return lumen_node_classes(handle_, o); });
    }
    /// Add a class (chainable).
    Node add_class(std::string_view cls) const {
        std::string c(cls);
        lumen_node_class_add(handle_, c.c_str());
        return *this;
    }
    /// Remove a class (chainable).
    Node remove_class(std::string_view cls) const {
        std::string c(cls);
        lumen_node_class_remove(handle_, c.c_str());
        return *this;
    }
    /// Toggle a class (chainable).
    Node toggle_class(std::string_view cls) const {
        std::string c(cls);
        lumen_node_class_toggle(handle_, c.c_str());
        return *this;
    }

    /// Serialize this node's children to `.lmn`-ish text (`innerHTML` read).
    std::string inner_markup() const {
        char* p = nullptr;
        lumen_node_inner_markup(handle_, &p);
        return detail::take_owned_string(p);
    }
    /// Serialize this subtree to `.lmn`-ish text (`outerHTML` read).
    std::string outer_markup() const {
        char* p = nullptr;
        lumen_node_outer_markup(handle_, &p);
        return detail::take_owned_string(p);
    }
    /// Replace this node's children with the subtree parsed from `markup`
    /// (`innerHTML` write, chainable).
    ///
    /// Guarded: parsing needs the injected markup front-end, present on the
    /// from-source run path and a no-op on the precompiled-artifact path. Do
    /// NOT feed untrusted content -- this injects live markup (XSS-adjacent).
    Node set_inner_markup(std::string_view markup) const {
        std::string m(markup);
        lumen_node_set_inner_markup(handle_, m.c_str());
        return *this;
    }

    // -- inline style (design 4.5) --------------------------------------

    /// The `element.style` override map.
    std::vector<std::pair<std::string, std::string>> inline_style() const {
        return detail::read_kvlist(
            [&](LumenKVList* o) { return lumen_node_inline_style(handle_, o); });
    }
    /// Read one inline style property (convenience over `inline_style()`).
    std::optional<std::string> style(std::string_view name) const {
        return detail::lookup(inline_style(), name);
    }
    /// Set an inline style property (chainable).
    Node set_style(std::string_view name, std::string_view value) const {
        std::string n(name), v(value);
        lumen_node_set_style(handle_, n.c_str(), v.c_str());
        return *this;
    }
    /// Remove an inline style property (chainable).
    Node remove_style(std::string_view name) const {
        std::string n(name);
        lumen_node_remove_style(handle_, n.c_str());
        return *this;
    }
    /// Every resolved CSS property after the cascade, keyed by name.
    std::vector<std::pair<std::string, std::string>> computed_style() const {
        return detail::read_kvlist(
            [&](LumenKVList* o) { return lumen_node_computed_style(handle_, o); });
    }

    // -- structure (design 4.3) -----------------------------------------

    /// Append `child` under this node (`appendChild`, chainable).
    Node append(Node child) const {
        lumen_node_append(handle_, child.handle_);
        return *this;
    }
    /// Insert `child` before `reference` under this node (`insertBefore`,
    /// chainable).
    Node insert_before(Node child, Node reference) const {
        lumen_node_insert_before(handle_, child.handle_, reference.handle_);
        return *this;
    }
    /// Attach this node under `parent` (reparent, chainable).
    Node set_parent(Node parent) const {
        lumen_node_set_parent(handle_, parent.handle_);
        return *this;
    }
    /// Replace this node with `other` in the parent, despawning this
    /// subtree. Returns `other`.
    Node replace_with(Node other) const {
        lumen_node_replace_with(handle_, other.handle_);
        return other;
    }
    /// Detach and despawn this node and its subtree (`remove`). Terminal.
    void remove() const { lumen_node_remove(handle_); }
    /// Deep-clone this subtree into a fresh detached node (`cloneNode(true)`).
    Node clone_deep() const {
        LumenNode out = 0;
        lumen_node_clone(handle_, &out);
        return Node(out);
    }

    // -- introspection (design 4.7) -------------------------------------

    /// Post-layout border-box, local + client (`getBoundingClientRect`).
    std::optional<Rect> rect() const { return out_rect(lumen_node_rect); }
    /// Content-box rect (inner box minus padding + border).
    std::optional<Rect> content_rect() const { return out_rect(lumen_node_content_rect); }

    /// Scroll offsets and their limits.
    std::optional<Scroll> scroll() const {
        LumenScroll s{};
        if (lumen_node_scroll(handle_, &s) != LUMEN_OK) return std::nullopt;
        return Scroll{s.x, s.y, s.max_x, s.max_y};
    }
    /// Effective visibility after `Visible(false)` / `display:none`.
    bool is_visible() const {
        int out = 0;
        return lumen_node_is_visible(handle_, &out) == LUMEN_OK && out != 0;
    }
    /// Resolved stacking order.
    int z_index() const {
        int out = 0;
        lumen_node_z_index(handle_, &out);
        return out;
    }
    /// The raw `(index, generation)` for debugging / handle round-trip.
    std::optional<std::pair<std::uint32_t, std::uint32_t>> entity_id() const {
        std::uint32_t index = 0, gen = 0;
        if (lumen_node_entity_id(handle_, &index, &gen) != LUMEN_OK) return std::nullopt;
        return std::make_pair(index, gen);
    }
    /// Names of the whitelisted Lumen components present on this node.
    std::vector<std::string> components() const {
        return detail::read_strlist(
            [&](LumenStrList* o) { return lumen_node_components(handle_, o); });
    }
    /// One component's public fields as a `(key, value)` map. Empty when the
    /// component is absent or not whitelisted.
    std::vector<std::pair<std::string, std::string>> component(std::string_view name) const {
        std::string n(name);
        return detail::read_kvlist(
            [&](LumenKVList* o) { return lumen_node_component(handle_, n.c_str(), o); });
    }

    // -- events (design 4.6) --------------------------------------------

    /// Bind `handler` to this node for `event_type` (bubble / target phase).
    /// Returns a `Listener`; call `Listener::off` to unbind. The callable is
    /// heap-anchored for the program's duration (until `off`), so the
    /// `Listener` need not outlive the binding.
    Listener on(std::string_view event_type,
                std::function<void(const Event&)> handler) const;
    /// Bind a capture-phase listener.
    Listener on_capture(std::string_view event_type,
                        std::function<void(const Event&)> handler) const;

private:
    template <typename Fn>
    std::optional<Node> out_node(Fn&& fn) const {
        LumenNode out = 0;
        if (fn(handle_, &out) == LUMEN_OK && out != 0) return Node(out);
        return std::nullopt;
    }
    template <typename Fn>
    std::optional<Rect> out_rect(Fn&& fn) const {
        LumenRect r{};
        if (fn(handle_, &r) != LUMEN_OK) return std::nullopt;
        return Rect{r.x, r.y, r.width, r.height, r.client_x, r.client_y};
    }

    Listener bind(std::string_view event_type, bool capture,
                  std::function<void(const Event&)> handler) const;

    LumenNode handle_ = 0;
};

namespace detail {

inline std::vector<Node> read_nodelist(LumenNodeList list) {
    std::vector<Node> out;
    out.reserve(list.len);
    for (std::size_t i = 0; i < list.len; ++i) out.emplace_back(Node(list.ptr[i]));
    lumen_nodelist_free(list);
    return out;
}

} // namespace detail

/// The event passed to a `Node::on` handler. Wraps the borrowed
/// `LumenEvent` snapshot; valid only for the duration of the handler call.
/// Scalar fields read the snapshot directly; the string fields
/// (`type` / `key` / `value`) fetch through the current-event C getters.
class Event {
public:
    explicit Event(const LumenEvent* ev) noexcept : ev_(ev) {}

    /// The node the event was dispatched to.
    Node target() const noexcept { return Node(ev_ ? ev_->target : lumen_event_target()); }
    /// The node whose listener is currently running.
    Node current_target() const noexcept {
        return Node(ev_ ? ev_->current_target : lumen_event_current_target());
    }

    /// The event type (`"click"`, `"keydown"`, ...).
    std::string type() const { return read_str(lumen_event_type); }
    /// The key for key events.
    std::string key() const { return read_str(lumen_event_key); }
    /// The value for input / change events.
    std::string value() const { return read_str(lumen_event_value); }

    /// Pointer position local to the target `(x, y)`.
    std::pair<double, double> position() const {
        return ev_ ? std::make_pair(ev_->local_x, ev_->local_y) : std::make_pair(0.0, 0.0);
    }
    /// Pointer position in window coordinates `(x, y)`.
    std::pair<double, double> client_position() const {
        return ev_ ? std::make_pair(ev_->client_x, ev_->client_y) : std::make_pair(0.0, 0.0);
    }
    /// Wheel delta `(dx, dy)`.
    std::pair<double, double> delta() const {
        return ev_ ? std::make_pair(ev_->delta_x, ev_->delta_y) : std::make_pair(0.0, 0.0);
    }
    /// The button for pointer events (0 primary, 1 middle, 2 secondary, -1
    /// none).
    std::int64_t button() const noexcept { return ev_ ? ev_->button : -1; }

    /// Modifier state `(shift, ctrl, alt, super)`.
    std::array<bool, 4> modifiers() const noexcept {
        if (!ev_) return {false, false, false, false};
        return {ev_->shift != 0, ev_->ctrl != 0, ev_->alt != 0, ev_->super_ != 0};
    }

    /// Cancel the event's default action.
    void prevent_default() const { lumen_event_prevent_default(); }
    /// Stop propagation to the next node.
    void stop_propagation() const { lumen_event_stop_propagation(); }
    /// Stop the remaining handlers everywhere.
    void stop_immediate_propagation() const { lumen_event_stop_immediate_propagation(); }

private:
    template <typename Fn>
    static std::string read_str(Fn&& fn) {
        return raw::detail::read_string_out([&](char* buf, std::size_t len, std::size_t* out) {
                   return fn(buf, len, out);
               }).value_or(std::string());
    }

    const LumenEvent* ev_ = nullptr;
};

namespace detail {

using EventHandler = std::function<void(const Event&)>;

/// Process-lifetime registry of bound event handlers, keyed by off token.
/// `lumen_on` stores the raw `EventHandler*` as `user_data`; anchoring the
/// owning `shared_ptr` here keeps that pointer valid regardless of what
/// happens to the `Listener`. `off` drops the anchor. The `inline`
/// function's `static` local is one instance across all TUs.
inline std::vector<std::pair<LumenEventToken, std::shared_ptr<EventHandler>>>& event_anchors() {
    static std::vector<std::pair<LumenEventToken, std::shared_ptr<EventHandler>>> v;
    return v;
}

/// C-callable bridge for `lumen_on`. Never lets a C++ exception escape into
/// the Rust FFI frames (that would be UB).
inline void event_trampoline(const LumenEvent* ev, void* user) noexcept {
    try {
        auto* fn = reinterpret_cast<EventHandler*>(user);
        (*fn)(Event{ev});
    } catch (...) {
        // Swallow -- unwinding across the C boundary is UB.
    }
}

inline void drop_event_anchor(LumenEventToken token) {
    auto& anchors = event_anchors();
    for (auto it = anchors.begin(); it != anchors.end(); ++it) {
        if (it->first == token) {
            anchors.erase(it);
            return;
        }
    }
}

} // namespace detail

/// A bound event listener. Call `Listener::off` to unbind
/// (`removeEventListener`). Destroying a `Listener` does NOT unbind -- the
/// handler stays anchored until `off`.
class Listener {
public:
    Listener() noexcept = default;
    explicit Listener(LumenEventToken token) noexcept : token_(token) {}

    /// The raw off token.
    LumenEventToken token() const noexcept { return token_; }

    /// Unbind the listener and drop its anchored handler.
    void off() {
        if (token_ == 0) return;
        lumen_off(token_);
        detail::drop_event_anchor(token_);
        token_ = 0;
    }

private:
    LumenEventToken token_ = 0;
};

inline Listener Node::bind(std::string_view event_type, bool capture,
                           std::function<void(const Event&)> handler) const {
    auto held = std::make_shared<detail::EventHandler>(std::move(handler));
    std::string t(event_type);
    LumenEventToken token = lumen_on(handle_, t.c_str(), capture ? 1 : 0,
                                     &detail::event_trampoline, held.get());
    if (token != 0) detail::event_anchors().emplace_back(token, std::move(held));
    return Listener(token);
}

inline Listener Node::on(std::string_view event_type,
                         std::function<void(const Event&)> handler) const {
    return bind(event_type, false, std::move(handler));
}

inline Listener Node::on_capture(std::string_view event_type,
                                 std::function<void(const Event&)> handler) const {
    return bind(event_type, true, std::move(handler));
}

// -- free entry points (design 4.1, 4.7) --------------------------------

/// Run a CSS selector query over the whole tree.
inline std::vector<Node> query(std::string_view selector) {
    std::string s(selector);
    LumenNodeList list{};
    if (lumen_query(s.c_str(), &list) != LUMEN_OK) return {};
    return detail::read_nodelist(list);
}

/// The single match, or `std::nullopt` for zero / many (`query_single`).
inline std::optional<Node> query_single(std::string_view selector) {
    std::string s(selector);
    LumenNode out = 0;
    if (lumen_query_single(s.c_str(), &out) == LUMEN_OK && out != 0) return Node(out);
    return std::nullopt;
}

/// Fast id lookup (`getElementById`).
inline std::optional<Node> get_by_id(std::string_view id) {
    std::string i(id);
    LumenNode out = 0;
    if (lumen_get_by_id(i.c_str(), &out) == LUMEN_OK && out != 0) return Node(out);
    return std::nullopt;
}

/// The root element (`document.documentElement`).
inline std::optional<Node> document() {
    LumenNode out = 0;
    if (lumen_document(&out) == LUMEN_OK && out != 0) return Node(out);
    return std::nullopt;
}

/// Create a fresh detached element with markup `tag` (`createElement`).
/// Attach it with `Node::append` / `Node::set_parent`.
inline Node spawn(std::string_view tag) {
    std::string t(tag);
    LumenNode out = 0;
    lumen_node_spawn(t.c_str(), &out);
    return Node(out);
}

/// Whole-tree structural dump (id / tag / classes / rect). An inspection call.
inline std::string dump_tree() {
    char* p = nullptr;
    lumen_dump_tree(&p);
    return detail::take_owned_string(p);
}

/// The whole signal set as `(name, value)` pairs. An inspection call.
inline std::vector<std::pair<std::string, std::string>> signals_all() {
    return detail::read_kvlist([&](LumenKVList* o) { return lumen_signals_all(o); });
}

/// Current pointer state snapshot.
inline std::optional<LumenPointerState> pointer_state() {
    LumenPointerState s{};
    if (lumen_pointer_state(&s) != LUMEN_OK) return std::nullopt;
    return s;
}

/// Current per-frame counters.
inline std::optional<LumenFrameInfo> frame_info() {
    LumenFrameInfo f{};
    if (lumen_frame_info(&f) != LUMEN_OK) return std::nullopt;
    return f;
}

} // namespace dom

} // namespace lumen

#endif // LUMEN_SDK_HPP
