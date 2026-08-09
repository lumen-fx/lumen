//! Headless smoke for a tree-shaken runtime (Part B).
//!
//! Builds a trivial precompiled LMNA artifact by hand (no markup parser, no
//! optional subsystem) and ticks it through `run_app_headless`. Run under
//! different `--features` sets to prove the trimmed runtime, and each
//! optional subsystem when enabled, still builds and ticks an app headless,
//! never opening a real window:
//!
//! ```text
//! # minimal (no audio/mcp/async/host-lua/host-candela):
//! cargo test -p lumen-runtime --no-default-features --features runtime-parse --test headless_min
//! # audio only:
//! cargo test -p lumen-runtime --no-default-features --features runtime-parse,audio --test headless_min
//! # full:
//! cargo test -p lumen-runtime --test headless_min
//! ```

use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::LayoutIR;
use lumen_runtime::{RunOptions, run_app_headless};

/// A hand-built minimal artifact (default root element, no script) ticks
/// cleanly headless regardless of which optional subsystems were compiled in.
#[test]
fn minimal_artifact_ticks_headless() {
    let dir = std::env::temp_dir().join(format!(
        "lumen_headless_min_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let app = CompiledApp {
        ir: LayoutIR::default(),
        script_source: String::new(),
    };
    let bytes = artifact::serialize(&app).expect("serialize trivial artifact");

    // `with_artifact_bytes` takes the parser-free link-not-embed path, so this
    // exercises a runtime built without the markup parser too.
    let opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    run_app_headless(opts, 3).expect("trimmed runtime ticks a trivial app headless");

    let _ = std::fs::remove_dir_all(&dir);
}
