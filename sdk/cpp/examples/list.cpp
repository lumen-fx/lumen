// list - a `<for>` array signal driven from C++.
//
// `<for each="rows">` markup renders one row per entry of an array signal.
// The host builds the rows as a `lumen::Value::array` of `Value::map`
// records and pushes them with `lumen::raw::set_array`. Array signals are
// the one part of the surface the typed `Signal<T>` layer deliberately
// doesn't cover (they aren't scalars), so this uses the `raw` layer.
//
// Runs headless and reads the rows back to confirm they landed.

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/list"
#endif

static lumen::Value row(const std::string& id, const std::string& name,
                        const std::string& role) {
    return lumen::Value::map({
        {"id",   lumen::Value::string(id)},
        {"name", lumen::Value::string(name)},
        {"role", lumen::Value::string(role)},
    });
}

int main(int argc, char** argv) {
    std::string dir = (argc > 1) ? argv[1] : LUMEN_APP_DIR;

    try {
        lumen::App app(dir, {.title = "Lumen - C++ List"});

        lumen::raw::set_array("rows", lumen::Value::array({
            row("1", "Ada",    "Analyst"),
            row("2", "Grace",  "Admiral"),
            row("3", "Linus",  "Maintainer"),
        }));

        app.run_headless(1);

        auto n = lumen::raw::array_len("rows").value_or(0);
        std::printf("list: %zu rows pushed\n", n);
        for (std::size_t i = 0; i < n; ++i) {
            auto name = lumen::raw::array_field("rows", i, "name").value_or("?");
            auto role = lumen::raw::array_field("rows", i, "role").value_or("?");
            std::printf("  row %zu: %s - %s\n", i, name.c_str(), role.c_str());
        }
        std::puts(n == 3 ? "list: OK" : "list: unexpected row count");
        return n == 3 ? 0 : 1;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "list: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
