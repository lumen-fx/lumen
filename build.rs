//! Build script: regenerate `include/lumen_simple.h` from the Rust
//! source via cbindgen.
//!
//! See `cbindgen.toml` for the configured surface. The complex
//! LumenValue tree types stay hand-written in `include/lumen.h` (which
//! `#include`s `lumen_simple.h`).
//!
//! The script is best-effort: if cbindgen fails (e.g. parser changes
//! upstream), we emit a `cargo:warning=` and continue - the hand-
//! written `lumen.h` still ships, so embedders aren't blocked. This
//! preserves the audit's stance that fighting cbindgen on the value
//! tree is more pain than win.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=lumen.pc.in");

    let crate_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let crate_dir = PathBuf::from(crate_dir);
    let out_path = crate_dir.join("include").join("lumen_simple.h");

    let config = match cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")) {
        Ok(c) => c,
        Err(e) => {
            println!("cargo:warning=lumen: cbindgen.toml unreadable: {e}");
            return;
        }
    };

    let builder = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config);
    match builder.generate() {
        Ok(bindings) => {
            // `write_to_file` only writes when the contents change, so
            // unchanged builds don't touch the file's mtime - handy for
            // incremental C++ builds depending on `lumen_simple.h`.
            bindings.write_to_file(&out_path);
        }
        Err(e) => {
            println!(
                "cargo:warning=lumen: cbindgen generation failed: {e}; keeping prior `lumen_simple.h`"
            );
        }
    }

    // --- pkg-config (W7.x) ---------------------------------------------
    //
    // Render `lumen.pc.in` into `OUT_DIR/lumen.pc` so downstream consumers
    // can `pkg-config --cflags --libs lumen` once the library is installed.
    //
    // `@PREFIX@` defaults to the env var `LUMEN_PREFIX` (if set) or
    // `/usr/local` - embedders / distributors that install into a
    // different prefix can set `LUMEN_PREFIX` before invoking cargo, or
    // hand-edit the generated file post-install. (A meson/cmake install
    // step is the right long-term home for prefix substitution; this
    // build.rs is the lightweight first cut.)
    let template_path = crate_dir.join("lumen.pc.in");
    let out_dir = match env::var("OUT_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => {
            println!("cargo:warning=lumen: OUT_DIR unset; skipping lumen.pc generation");
            return;
        }
    };
    let pc_path = out_dir.join("lumen.pc");
    match fs::read_to_string(&template_path) {
        Ok(template) => {
            let prefix = env::var("LUMEN_PREFIX").unwrap_or_else(|_| "/usr/local".to_string());
            let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
            let rendered = template
                .replace("@PREFIX@", &prefix)
                .replace("@VERSION@", &version);
            if let Err(e) = fs::write(&pc_path, &rendered) {
                println!(
                    "cargo:warning=lumen: failed to write {}: {e}",
                    pc_path.display()
                );
            } else {
                println!(
                    "cargo:warning=lumen: wrote {} (set LUMEN_PREFIX to override prefix={prefix})",
                    pc_path.display()
                );
            }
        }
        Err(e) => {
            println!(
                "cargo:warning=lumen: lumen.pc.in unreadable: {e}; skipping lumen.pc generation"
            );
        }
    }
}
