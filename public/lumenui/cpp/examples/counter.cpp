// counter - the ergonomic surface: a typed Signal, a derived label, and
// 0-arg native click handlers.
//
// This mirrors the Python SDK's counter example. `count` is a
// `Signal<int64_t>`; `count += 1` writes and `*count` reads. The `label`
// signal is *derived* from `count` with `watch` (the C++ analogue of the
// Python `computed`), so the bound `bind-text="label"` markup stays in
// sync without a manual refresh.
//
// Native clicks do not fire under `run_headless` (no input is injected),
// so this runs a couple of ticks to prove the wiring builds and the
// reactive derive fires, then exits. Pass `--window` to click for real.

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/counter"
#endif

static std::string label_for(std::int64_t n) {
    return std::to_string(n) + (n == 1 ? " click" : " clicks");
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
        lumen::App app(dir, {.title = "Lumen - C++ Counter"});

        lumen::Signal<std::int64_t> count{"count", 0};
        lumen::Signal<std::string>  label{"label", label_for(0)};

        // Derive `label` from `count`: whenever the count commits on a tick,
        // recompute and push the text. The reactive graph keeps them in sync.
        count.watch([label](std::int64_t n) mutable { label = label_for(n); });

        // Ergonomic 0-arg click handlers - no `id` parameter needed. The
        // runtime routes each element id straight here (ABI 0.3).
        app.on_click("increment", [&] { count += 1; });
        app.on_click("decrement", [&] { count -= 1; });
        app.on_click("reset",     [&] { count = 0; });

        // Graceful-shutdown hook (ABI 0.5): allow the close, log on the way out.
        app.on_close([] { std::puts("counter: window closing"); return true; });

        if (window) {
            return app.run();
        }

        // Headless: clicks don't fire, but we can drive the signal directly to
        // show the typed operators + the derived label updating over ticks.
        count = 41;
        app.run_headless(2);
        std::printf("counter: count=%lld, label=\"%s\" (derived). OK\n",
                    static_cast<long long>(*count), label.get().c_str());
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "counter: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
