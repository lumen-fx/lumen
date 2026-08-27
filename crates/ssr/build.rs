//! Compiles the test fixtures to `.cdlb` bytecode.
//!
//! A render installs the candela host, which carries no compiler, so the
//! suite needs an image from somewhere. Producing it here with the same
//! `CandelaHost::compile_bytecode` a `lumenc build` calls means a candela
//! artifact format bump breaks the test loudly instead of leaving a
//! checked-in blob to rot.
//!
//! Build scripts are compiled and run for the host, so the compiler this
//! links is never part of a shipped renderer.

use std::env;
use std::fs;
use std::path::Path;

/// The fixtures, by source file stem.
const FIXTURES: &[&str] = &["reads_request", "answers", "fetches", "components"];

fn main() {
    // The fixtures are candela, and the suite that loads them runs only when
    // that host is compiled in.
    if env::var_os("CARGO_FEATURE_HOST_CANDELA").is_none() {
        return;
    }
    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for stem in FIXTURES {
        let source_path = Path::new("fixtures").join(format!("{stem}.cdl"));
        println!("cargo::rerun-if-changed={}", source_path.display());
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", source_path.display()));
        // These fixtures import no native library and register no module or
        // plugin function, so a bare host with nothing folded in is enough.
        let image = lumen_script_candela::CandelaHost::new()
            .compile_bytecode(&source, &source_path.to_string_lossy())
            .unwrap_or_else(|e| panic!("compiling {}: {e}", source_path.display()));
        let out_path = Path::new(&out_dir).join(format!("{stem}.cdlb"));
        fs::write(&out_path, image)
            .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
    }
}
