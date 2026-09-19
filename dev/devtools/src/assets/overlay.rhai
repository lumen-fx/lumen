// Lumen devtools overlay - reference interaction script.
//
// This documents the intended, dogfooded form of the overlay's tab
// switching through the reactive signal bus. It is shipped as an embedded
// asset for parity with the .lmn + .css authoring stack.
//
// v1 note: the overlay is co-mounted on top of the running app, which
// already owns the single Rhai script host. Running a *second*, isolated
// host for the overlay needs the multi-host script registry (see the
// project's multi-host scripting plan) and is post-v1. Until then the same
// behaviour is driven by `lumen-devtools`' Rust systems, which write the
// exact same `dt_tab` signal this script would - so the data path
// (click -> dt_tab signal -> body refresh) is identical.

fn on_start() {
    // Default to the Elements tab.
    signal("dt_tab", "elements");
}

// Tab buttons carry ids dt-tab-elements / dt-tab-signals / dt-tab-network.
fn on_click(id) {
    if id == "dt-tab-elements" { signal("dt_tab").set("elements"); }
    if id == "dt-tab-signals"  { signal("dt_tab").set("signals"); }
    if id == "dt-tab-network"  { signal("dt_tab").set("network"); }
}
