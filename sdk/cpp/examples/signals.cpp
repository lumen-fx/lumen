// signals - the typed Signal<T> surface end to end.
//
// Exercises every supported scalar T (int64, double, bool, std::string,
// Color) through read/write, the += / -= operators, Color hex parsing
// (`Color::from_hex`, the C++ parity for Python's `Color("#ff8000")`), and
// a derived value via `watch`. Typed scalar signals round-trip through the
// FFI-local cache, so most reads work even before the app ticks; the
// derived value uses `watch`, which fires during `run_headless` ticks.
//
// Runs headless - no window needed. Pass a directory to point elsewhere.

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/counter"
#endif

int main(int argc, char** argv) {
    std::string dir = (argc > 1) ? argv[1] : LUMEN_APP_DIR;

    try {
        lumen::App app(dir, {.title = "Lumen - C++ Signals"});

        // --- typed scalars: write, then read back, typed ------------------
        lumen::Signal<std::int64_t> hits{"hits", 0};
        hits += 5;
        hits -= 2;                                   // 3

        lumen::Signal<double> ratio{"ratio", 0.5};
        ratio = *ratio * 2.0;                        // 1.0

        lumen::Signal<bool> ready{"ready", false};
        ready = true;

        lumen::Signal<std::string> title{"title", "hello"};
        title += " world";                           // "hello world"

        // Color hex parsing - matches the Python SDK's Color("#ff8000").
        lumen::Signal<lumen::Color> tint{"tint", lumen::Color::from_hex("#ff8000")};

        std::printf("hits=%lld ratio=%.3f ready=%s title=\"%s\" tint=%s\n",
                    static_cast<long long>(*hits), *ratio,
                    *ready ? "true" : "false", title.get().c_str(),
                    tint.get().to_hex().c_str());

        // --- a derived signal via watch (the `computed` analogue) ---------
        lumen::Signal<std::string> summary{"summary"};
        hits.watch([summary](std::int64_t n) mutable {
            summary = std::to_string(n) + " hits";
        });

        hits = 7;
        app.run_headless(2);                         // let the watch fire
        std::printf("derived summary=\"%s\". OK\n", summary.get().c_str());
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "signals: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
