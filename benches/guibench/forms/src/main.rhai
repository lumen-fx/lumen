// forms bench app - widget-dense settings page (40 controls in 6
// groups; shared spec in the repo README).
//
// The interaction pass is driven entirely by the harness over MCP
// `lumen.simulate` Key events (Tab focus-walk + Space on the focused
// checkbox/toggle/radio): Lumen's Rhai API has no focus builtins, and
// Rhai timers only fire on ticks - the demand-driven headless loop
// parks between events, so a set_interval-driven pass would stall.
// Keyboard events go through the real input pipeline (focus chain,
// Space-activation FSM), which is the closest Lumen equivalent of the
// other frameworks' programmatic focus/toggle drivers.

fn on_start() {}
