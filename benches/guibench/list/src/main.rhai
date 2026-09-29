// Bench app - identical spec to the other five frameworks.
//
// Measurement notes (see guibench harness/bench.py):
// - Lumen's Rhai API has no per-frame callback today (`on_tick` is
//   documented in docs/src/authoring/rhai-builtins.md but not
//   implemented), and no scroll-offset setter. The harness therefore
//   measures Lumen externally over MCP: the `lumen.tick` method exposes
//   the frame counter and last-tick duration, and `lumen.simulate`
//   injects wheel events (sensitivity 1.0 / inertia 0 below makes one
//   wheel px == one scroll px).

fn on_start() {
    let rows = [];
    for i in 0..10000 {
        let s = "" + i;
        rows.push(#{ id: s, i: s });
    }
    signal_array("items").set(rows);

    let clicks = signal("clicks", 0);
    derive("count_label", [clicks], |n| "Count: " + n);
    derive("level_label", [signal("level", 50)], |v| "" + v);
}

fn on_click(id) {
    if id == "count" {
        let clicks = signal("clicks", 0);
        clicks.set(clicks.get() + 1);
    }
}
