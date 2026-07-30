// hello - the smallest possible Lumen C++ app.
//
// Constructs an `App` over an app directory (lumen.toml + main.lmn),
// prints the negotiated ABI version, and builds-then-drops it headless
// (`run_headless(0)`) so it needs no display. Pass `--window` to open a
// real OS window instead, or a directory to point at a different app.
//
//   ./hello                 # headless build-and-drop (CI / agent safe)
//   ./hello --window        # open a real window
//   ./hello path/to/app     # a different app directory

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/hello"
#endif

int main(int argc, char** argv) {
    std::string dir = LUMEN_APP_DIR;
    bool window = false;
    for (int i = 1; i < argc; ++i) {
        std::string a = argv[i];
        if (a == "--window") window = true;
        else dir = a;
    }

    std::printf("lumen C++ SDK - header ABI %u, runtime ABI %u, compatible=%s\n",
                lumen::header_abi_version(), lumen::runtime_abi_version(),
                lumen::abi_compatible() ? "yes" : "no");

    try {
        lumen::App app(dir, {.title = "Lumen - C++ Hello"});
        if (window) {
            return app.run();          // blocks until the window closes
        }
        app.run_headless(0);           // build the app, then drop it - no display
        std::puts("hello: app built and validated headless. OK");
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "hello: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
