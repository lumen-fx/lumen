// handlers - every click-handler shape the SDK accepts.
//
// `on_click(id, fn)` takes either:
//   * a 0-arg handler `[]{ ... }`            (you already know the id), or
//   * a 1-arg handler `[](std::string id)`   (receives the clicked id).
// Both are the "direct-register" form - you pass the callable inline; C++
// has no decorator syntax, so this single call IS the equivalent of the
// Python SDK's `app.on_click("id", fn)` direct-call form. A plain function
// pointer or a stored `std::function` works too.
//
// Native clicks are not injected under `run_headless`, so this proves the
// handlers register and link; pass `--window` to fire them for real.

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/counter"
#endif

// A free function used as a directly-registered handler.
static void on_reset(std::string id) {
    std::printf("handlers: reset via '%s'\n", id.c_str());
}

int main(int argc, char** argv) {
    std::string dir = LUMEN_APP_DIR;
    bool window = false;
    for (int i = 1; i < argc; ++i) {
        std::string a = argv[i];
        if (a == "--window") window = true;
        else dir = a;
    }

    try {
        lumen::App app(dir, {.title = "Lumen - C++ Handlers"});

        int clicks = 0;

        // 0-arg form: doesn't care which id fired.
        app.on_click("increment", [&] {
            ++clicks;
            std::printf("handlers: +1 (clicks=%d)\n", clicks);
        });

        // 1-arg form: receives the clicked element id.
        app.on_click("decrement", [&](std::string id) {
            --clicks;
            std::printf("handlers: -1 from '%s' (clicks=%d)\n", id.c_str(), clicks);
        });

        // Direct-register a plain function pointer.
        app.on_click("reset", &on_reset);

        // Close hook accepts bool() (veto-able) or void() (notify only).
        app.on_close([&] { std::printf("handlers: closing after %d clicks\n", clicks); return true; });

        if (window) {
            return app.run();
        }

        app.run_headless(1);
        std::puts("handlers: 0-arg, 1-arg, and function-pointer handlers registered. OK");
        std::puts("handlers: (run with --window to actually fire clicks)");
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "handlers: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
