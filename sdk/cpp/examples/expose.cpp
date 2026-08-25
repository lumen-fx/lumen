// expose - a native C++ function callable from the app's script.
//
// `app.expose(name, arity, fn)` registers a native builtin; the Rhai
// script in apps/expose/src/main.lmn calls `cpp_greeting("Lumen")` from its
// `on_start` hook, and the return value flows back across the FFI. The
// callback receives a `lumen::Args` view and returns a `lumen::Value`.
//
// Also shows the 0-arg convenience overload `expose(name, fn)` (arity 0).
// Runs headless: `run_headless` executes scripts, so `on_start` - and thus
// our exposed function - actually fires without a window.

#include <lumen.hpp>

#include <atomic>
#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/expose"
#endif

int main(int argc, char** argv) {
    std::string dir = (argc > 1) ? argv[1] : LUMEN_APP_DIR;

    try {
        lumen::App app(dir, {.title = "Lumen - C++ Host Builtin"});

        std::atomic<int> calls{0};

        // Arity-1 builtin: greet the name the script passes in.
        app.expose("cpp_greeting", 1, [&calls](lumen::Args args) -> lumen::Value {
            ++calls;
            std::string name = args.empty() ? std::string("world") : args.at(0).as_string();
            std::printf("expose: cpp_greeting(\"%s\") called from script\n", name.c_str());
            return lumen::Value::string("Hello, " + name + "!");
        });

        // 0-arg convenience overload: `expose(name, fn)` defaults to arity 0.
        app.expose("cpp_answer", [] { return lumen::Value::integer(42); });

        app.run_headless(2);   // runs on_start(), which calls cpp_greeting(...)

        if (calls.load() > 0) {
            std::puts("expose: script -> host dispatch confirmed. OK");
            return 0;
        }
        std::puts("expose: builtin registered but on_start did not call it");
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "expose: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
