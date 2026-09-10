//! The `mcp` capability: the introspection server.
//!
//! Whether it listens is decided here, from `lumen.toml` and the run mode:
//!
//! 1. `[mcp] port = 0` disables it outright.
//! 2. `[runtime] mcp = false` disables it; `= true` enables it even in a
//!    headless run.
//! 3. A headless run leaves it off unless `[mcp] simulate` is on, because
//!    the server thread and the per-tick snapshot pipeline are pure overhead
//!    for a tick bench; an automation driver that injects input sets
//!    `simulate = true` and keeps it.
//! 4. Otherwise, interactive, it listens on the configured port or 7878.
//!
//! `simulate` and `issues` are off unless asked for: `lumen_framework_status`
//! shells out to `git` and `gh` when issues are on, and the port has no
//! authentication, so a shipped app leaves subprocess execution off that
//! surface unless a developer opts in.

use std::time::Duration;

use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use serde::Deserialize;

use crate::{LumenMcpPlugin, McpSnapshotSchedule};

/// The `[mcp]` keys this capability reads.
#[derive(Default, Deserialize)]
struct McpSection {
    port: Option<u16>,
    simulate: Option<bool>,
    issues: Option<bool>,
}

/// The `[runtime]` key this capability reads.
#[derive(Default, Deserialize)]
struct RuntimeSection {
    mcp: Option<bool>,
}

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    let mcp: McpSection = env.section("mcp");
    let runtime: RuntimeSection = env.section("runtime");
    let simulate_enabled = mcp.simulate.unwrap_or(false);
    let issues_enabled = mcp.issues.unwrap_or(false);
    let enabled = match runtime.mcp {
        Some(v) => v,
        None => !(env.headless && !simulate_enabled),
    };
    let port: Option<u16> = match (enabled, mcp.port) {
        (false, _) | (_, Some(0)) => None,
        (true, Some(p)) => {
            app.add_plugin(
                LumenMcpPlugin::with_port(p)
                    .with_simulate_enabled(simulate_enabled)
                    .with_issues_enabled(issues_enabled),
            );
            Some(p)
        }
        (true, None) => {
            app.add_plugin(
                LumenMcpPlugin::default()
                    .with_simulate_enabled(simulate_enabled)
                    .with_issues_enabled(issues_enabled),
            );
            Some(7878)
        }
    };
    print_help_snippet(port, simulate_enabled, issues_enabled);

    // Snapshot cadence. Input-simulation automation (benchmarks, UI tests)
    // drives the app through the `lumen.simulate` queue and observes
    // progress through the snapshot frame counter and scroll-corrected
    // rects. The default 1 Hz throttle makes that observation useless: the
    // frame counter advances about once a second, and re-queried rects go
    // stale between a scroll-into-view nudge and the follow-up read. The
    // same holds for a headless run, whose ticks are on demand, so per-tick
    // snapshots are free there and make the frame-advance wait
    // deterministic. Passive introspection at a window keeps the throttle,
    // to spare a normal interactive app the per-frame sweep.
    if port.is_some() && (simulate_enabled || env.headless) {
        for world in [&mut app.world, &mut app.render_world] {
            if let Some(mut sched) = world.get_resource_mut::<McpSnapshotSchedule>() {
                sched.interval = Duration::ZERO;
            }
        }
    }
}

/// Print a copy-pasteable MCP setup hint on stdout. Designed for one-shot
/// scan by an AI agent: the port number on the first line, then a JSON
/// fragment ready to drop into a Claude Code `.mcp.json`.
fn print_help_snippet(port: Option<u16>, simulate_enabled: bool, issues_enabled: bool) {
    let Some(port) = port else {
        println!("lumenc: MCP server disabled");
        return;
    };
    let sim = if simulate_enabled {
        "ON"
    } else {
        "off - set [mcp] simulate = true in lumen.toml to enable input injection"
    };
    let issues = if issues_enabled {
        "ON"
    } else {
        "off - set [mcp] issues = true in lumen.toml to let lumen_framework_status list open issues"
    };
    println!("lumenc: MCP server on 127.0.0.1:{port} (simulate: {sim})");
    println!("        issue lookup: {issues}");
    println!("        try: lumenc snapshot --port {port}");
    println!("        Claude Code config snippet (drop into .mcp.json under \"mcpServers\"):");
    println!("        \"lumen\": {{");
    println!("          \"command\": \"lumen-mcp-server\",");
    println!("          \"args\": [\"--host\", \"127.0.0.1\", \"--port\", \"{port}\"]");
    println!("        }}");
}
