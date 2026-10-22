// dom - the dynamic DOM surface (design 4.1-4.8) over the C ABI.
//
// `lumen::dom` wraps the DOM-parity C exports into thin `Node` handles:
// query and traverse the tree, read and write attributes / classes / text /
// inline style, build and rearrange nodes, read post-layout geometry, and
// bind event handlers. Every call maps onto one C export; reads are soft (a
// stale handle reads back an empty optional), and mutations queue on the
// command bus the app drains once per tick.
//
// This example builds a detached subtree, sets attributes / classes / text
// and `inner_markup`, and binds a click listener - proving the whole
// surface compiles, links, and applies without crashing under
// `run_headless`. The queued edits drain during the headless ticks; the
// snapshot reads (query / rect / computed_style) only return live data once
// a window is up, so pass `--window` to inspect them for real.

#include <lumen.hpp>

#include <cstdio>
#include <string>

#ifndef LUMEN_APP_DIR
#define LUMEN_APP_DIR "apps/counter"
#endif

int main(int argc, char** argv) {
    std::string dir = LUMEN_APP_DIR;
    bool window = false;
    for (int i = 1; i < argc; ++i) {
        std::string a = argv[i];
        if (a == "--window") window = true;
        else dir = a;
    }

    try {
        lumen::App app(dir, {.title = "Lumen - C++ DOM"});

        namespace dom = lumen::dom;

        // Build a detached subtree: <row class="item" data-id="1"><label/></row>.
        dom::Node row = dom::spawn("row")
                            .set_attr("data-id", "1")
                            .add_class("item")
                            .set_text("row 1");
        dom::Node label = dom::spawn("label").set_text("label");
        row.append(label);

        // Replace the row's children from a markup fragment (guarded).
        row.set_inner_markup("<label>replaced</label>");

        // Bind a click listener - anchored until `.off()`, fires with a window.
        dom::Listener click = row.on("click", [](const dom::Event& e) {
            std::printf("dom: click on node 0x%llx (%s)\n",
                        static_cast<unsigned long long>(e.target().handle()),
                        e.type().c_str());
        });

        if (window) {
            LumenStatus st = app.run();
            click.off();
            return st;
        }

        app.run_headless(2);
        std::printf("dom: built row (handle 0x%llx) + child, bound click. OK\n",
                    static_cast<unsigned long long>(row.handle()));
        std::puts("dom: (run with --window to query / read geometry live)");
        return 0;
    } catch (const lumen::Error& e) {
        std::fprintf(stderr, "dom: %s (status %d)\n", e.what(), e.status());
        return 1;
    }
}
