//! Puts the candela standard library beside a `lumenc` installed from
//! crates.io; see `src/script_library_install.rs`. In a checkout it does
//! nothing.
//!
//! The published `lumenc` runs the candela host's build script ahead of this
//! one (tools/release/bundle-crate.py), so the tree is staged by the time this
//! looks for it.

use std::path::PathBuf;

#[path = "src/script_library_install.rs"]
mod script_library_install;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_INSTALL_ROOT");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    if let Err(cause) =
        script_library_install::place(&manifest_dir, &out_dir, |key: &str| std::env::var_os(key))
    {
        println!(
            "cargo:warning=lumenc: {}",
            script_library_install::not_placed(&cause)
        );
    }
}
