//! Compiles the browser test fixtures to `.cdlb` bytecode.
//!
//! The runtime this crate builds carries no compiler, so a test needs an image
//! from somewhere. Producing it here, with the same `compile_bytecode` a
//! `lumenc build` calls, means the artifact a browser loads is always the one
//! this toolchain emits: a candela artifact format bump breaks the test loudly
//! instead of leaving a checked-in blob to rot.
//!
//! Build scripts are compiled and run for the host, so the compiler this links
//! is never part of the wasm module.
//!
//! The sources are the ones `lumen-portable` keeps beside its host modules.
//! The browser suite and the host suite must load the same programs, and one
//! copy is how they stay the same.

use std::env;
use std::fs;
use std::path::Path;

/// Where the fixture sources live, relative to this package.
const FIXTURE_DIR: &str = "../portable/fixtures";

/// The fixtures, by source file stem. `unbound` compiles and fails to load,
/// on purpose. `fetch` reaches the network, so only a suite with a browser and
/// a server around it can run it.
const FIXTURES: &[&str] = &["components", "fetch", "smoke", "unbound"];

fn main() {
    // The fixtures are candela, and the suite that loads them runs only when
    // that host is compiled in.
    if env::var_os("CARGO_FEATURE_HOST_CANDELA").is_none() {
        return;
    }
    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for stem in FIXTURES {
        let source_path = Path::new(FIXTURE_DIR).join(format!("{stem}.cdl"));
        println!("cargo::rerun-if-changed={}", source_path.display());
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", source_path.display()));
        let image = lumen_script_candela::compile_bytecode(&source, &source_path.to_string_lossy())
            .unwrap_or_else(|e| panic!("compiling {}: {e}", source_path.display()));
        let out_path = Path::new(&out_dir).join(format!("{stem}.cdlb"));
        fs::write(&out_path, image)
            .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
    }
}
