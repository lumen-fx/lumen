// Lumen C++17 header-only wrapper over lumen.h.
//
// Goals:
//   - RAII for LumenApp (auto-free on exception)
//   - Builder syntax: App(".").expose(...).run()
//   - Lambda / std::function callbacks (auto-wrapped to LumenFn)
//   - Owned lumen::Value type with static factories that handle
//     buffer lifetime (Value owns its strings / arrays / maps; the
//     temporary LumenValue it materialises borrows from itself for
//     the duration of one C ABI call)
//   - Direct signal mutators that don't touch the script
//
// Threading: lumen::Signal::* helpers are thread-safe - the
// underlying lumen_signal_set_* calls go through a thread-safe
// channel into the Lumen ECS world. Callbacks fire on the Lumen
// script thread.
//
// This header includes the C ABI declarations from <lumen.h>; do
// not pre-include it.

#ifndef LUMEN_HPP
#define LUMEN_HPP

#pragma once

#include "lumen.h"

#include <cstdint>
#include <cstring>
#include <functional>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

namespace lumen {

// =====================================================================
// Error
// =====================================================================

class Error : public std::runtime_error {
public:
    explicit Error(std::string msg, LumenStatus status = LUMEN_ERR_RUNTIME)
        : std::runtime_error(std::move(msg)), status_(status) {}
    LumenStatus status() const noexcept { return status_; }
private:
    LumenStatus status_;
};

inline std::string last_error() {
    const char* p = lumen_last_error();
    return p ? std::string(p) : std::string();
}

inline void check(LumenStatus s, const char* op) {
    if (s != LUMEN_OK) throw Error(std::string(op) + ": " + last_error(), s);
}

// =====================================================================
// Value - owned, recursive
//
// One owned value, with an in-place conversion to a `LumenValue` that
// borrows from `self`. The borrowed view stays valid only while
// `*this` is alive and not moved.
// =====================================================================

class Value {
public:
    // Constructors / factories
    Value() noexcept = default;
    static Value nil()                              { return Value{}; }
    static Value boolean(bool v)                    { Value r; r.kind_ = LUMEN_BOOL;   r.bool_ = v; return r; }
    static Value integer(int64_t v)                 { Value r; r.kind_ = LUMEN_INT;    r.int_ = v;  return r; }
    static Value floating(double v)                 { Value r; r.kind_ = LUMEN_FLOAT;  r.float_ = v; return r; }
    static Value string(std::string v)              { Value r; r.kind_ = LUMEN_STRING; r.str_ = std::move(v); return r; }
    static Value array(std::vector<Value> v)        { Value r; r.kind_ = LUMEN_ARRAY;  r.array_ = std::move(v); return r; }
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
    bool        as_bool()    const  { return bool_; }
    int64_t     as_int()     const  { return int_; }
    double      as_float()   const  { return float_; }
    const std::string& as_string() const { return str_; }
    const std::vector<Value>& as_array() const { return array_; }
    const std::vector<std::pair<std::string, Value>>& as_map() const { return map_; }

    // Materialise into a borrowed LumenValue tree. The caller-provided
    // `view_arena` and `entry_arena` keep child views alive while the
    // returned LumenValue is in use; do not let them go out of scope
    // before passing the LumenValue across the ABI.
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

    // Read-only adapter: borrow from a LumenValue (e.g. an argv slot)
    // into a heap-owned Value tree.
    static Value adopt(const LumenValue& src) {
        switch (src.kind) {
            case LUMEN_NIL:    return Value::nil();
            case LUMEN_BOOL:   return Value::boolean(src.as_.boolean != 0);
            case LUMEN_INT:    return Value::integer(src.as_.integer);
            case LUMEN_FLOAT:  return Value::floating(src.as_.float_);
            case LUMEN_STRING: return Value::string(src.as_.string ? std::string(src.as_.string) : std::string());
            case LUMEN_ARRAY: {
                std::vector<Value> out;
                out.reserve(src.as_.array.len);
                for (size_t i = 0; i < src.as_.array.len; ++i) {
                    out.push_back(Value::adopt(src.as_.array.items[i]));
                }
                return Value::array(std::move(out));
            }
            case LUMEN_MAP: {
                std::vector<std::pair<std::string, Value>> out;
                out.reserve(src.as_.map.len);
                for (size_t i = 0; i < src.as_.map.len; ++i) {
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
    LumenKind kind_ = LUMEN_NIL;
    bool        bool_  = false;
    int64_t     int_   = 0;
    double      float_ = 0.0;
    std::string str_;
    std::vector<Value> array_;
    std::vector<std::pair<std::string, Value>> map_;
};

// =====================================================================
// Args - thin wrapper around (argc, argv) inside a callback.
// =====================================================================

class Args {
public:
    Args(int argc, const LumenValue* argv) noexcept : argc_(argc), argv_(argv) {}
    int    size() const noexcept { return argc_; }
    bool   empty() const noexcept { return argc_ == 0; }
    const LumenValue& operator[](int i) const noexcept { return argv_[i]; }
    Value  at(int i) const { return Value::adopt(argv_[i]); }
private:
    int argc_;
    const LumenValue* argv_;
};

// =====================================================================
// App - RAII handle around LumenApp*. Builder methods return *this.
// =====================================================================

class App {
public:
    // Most callbacks need no captures - accept a plain function-like
    // type. Stored as std::function so lambdas with captures work.
    using Callback = std::function<Value(Args)>;

    explicit App(std::string_view dir) {
        std::string z(dir);
        raw_ = lumen_app_new(z.c_str());
        if (!raw_) throw Error("lumen_app_new failed: " + last_error(), LUMEN_ERR_BAD_PATH);
    }

    App(const App&) = delete;
    App& operator=(const App&) = delete;

    App(App&& other) noexcept
        : raw_(other.raw_), callbacks_(std::move(other.callbacks_)) {
        other.raw_ = nullptr;
    }
    App& operator=(App&& other) noexcept {
        if (this != &other) {
            release();
            raw_ = other.raw_;
            callbacks_ = std::move(other.callbacks_);
            other.raw_ = nullptr;
        }
        return *this;
    }

    ~App() { release(); }

    App& title(std::string_view t) {
        std::string z(t);
        check(lumen_app_set_title(raw_, z.c_str()), "lumen_app_set_title");
        return *this;
    }

    App& size(uint32_t w, uint32_t h) {
        check(lumen_app_set_size(raw_, w, h), "lumen_app_set_size");
        return *this;
    }

    // Expose a callable to the app's script. The callback may capture
    // freely; we move it onto the heap so the trampoline can find it
    // by `user_data` pointer for the lifetime of this App.
    App& expose(std::string_view name, uint32_t arg_count, Callback cb) {
        auto owned = std::make_unique<Callback>(std::move(cb));
        void* ud = owned.get();
        callbacks_.push_back(std::move(owned));
        std::string z(name);
        check(lumen_app_expose(raw_, z.c_str(), arg_count,
                               &App::trampoline, ud),
              "lumen_app_expose");
        return *this;
    }

    // Zero-arity convenience overload.
    template <typename F>
    App& expose(std::string_view name, F&& f) {
        return expose(name, /*arg_count=*/0, [fn = std::forward<F>(f)](Args args) -> Value {
            (void)args;
            if constexpr (std::is_invocable_r_v<Value, F>) {
                return fn();
            } else if constexpr (std::is_invocable_r_v<Value, F, Args>) {
                return fn(args);
            } else {
                static_assert(sizeof(F) == 0,
                              "expose(name, fn): fn must be Value() or Value(Args).");
            }
        });
    }

    // Consume + run. Returns when window closes.
    LumenStatus run() && {
        LumenApp* app = raw_;
        raw_ = nullptr;
        return lumen_app_run(app);
    }

private:
    void release() noexcept {
        if (raw_) { lumen_app_free(raw_); raw_ = nullptr; }
    }

    static LumenValue trampoline(int argc, const LumenValue* argv, void* user) noexcept {
        try {
            auto* cb = reinterpret_cast<Callback*>(user);
            Value out = (*cb)(Args{argc, argv});
            // Build a borrowed LumenValue. The arenas live until this
            // function returns - Lumen copies before we unwind, so
            // the borrowed pointers are valid for the call's lifetime.
            // We stash the owned `Value` plus arenas in a static
            // thread_local so the LumenValue we return can reference
            // their memory until the C ABI side finishes copying.
            // Two-buffer ping-pong handles the rare case of a callback
            // being called twice without Lumen copying in between
            // (it doesn't, today, but defensive).
            thread_local Value owners[2];
            thread_local std::vector<std::vector<LumenValue>> view_arenas[2];
            thread_local std::vector<std::vector<LumenMapEntry>> entry_arenas[2];
            thread_local unsigned slot = 0;
            slot ^= 1;
            owners[slot] = std::move(out);
            view_arenas[slot].clear();
            entry_arenas[slot].clear();
            return owners[slot].view(view_arenas[slot], entry_arenas[slot]);
        } catch (...) {
            LumenValue nil{};
            nil.kind = LUMEN_NIL;
            nil.as_.integer = 0;
            return nil;
        }
    }

    LumenApp* raw_ = nullptr;
    // std::function callbacks live here so trampoline can resolve
    // them by user_data pointer. unique_ptr keeps pointers stable
    // across vector growth.
    std::vector<std::unique_ptr<Callback>> callbacks_;
};

// =====================================================================
// Signal - thread-safe direct mutation, no script needed.
// =====================================================================

namespace Signal {

    inline void set(std::string_view name, std::string_view value) {
        std::string n(name), v(value);
        lumen_signal_set_str(n.c_str(), v.c_str());
    }
    inline void set(std::string_view name, int64_t value) {
        std::string n(name);
        lumen_signal_set_int64(n.c_str(), value);
    }
    inline void set(std::string_view name, double value) {
        std::string n(name);
        lumen_signal_set_float64(n.c_str(), value);
    }
    inline void set_array(std::string_view name, const Value& v) {
        std::string n(name);
        std::vector<std::vector<LumenValue>> va;
        std::vector<std::vector<LumenMapEntry>> ea;
        LumenValue lv = v.view(va, ea);
        lumen_signal_set_array(n.c_str(), &lv);
    }
    inline void clear(std::string_view name) {
        std::string n(name);
        lumen_signal_clear(n.c_str());
    }

} // namespace Signal

} // namespace lumen

#endif // LUMEN_HPP
