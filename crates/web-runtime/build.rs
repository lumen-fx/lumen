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

use std::env;
use std::fs;
use std::path::Path;

/// The fixtures, by source file stem. `unbound` compiles and fails to load,
/// on purpose.
const FIXTURES: &[&str] = &["smoke", "unbound"];

fn main() {
    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for stem in FIXTURES {
        let source_path = Path::new("fixtures").join(format!("{stem}.cdl"));
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
