//! Points the compiler at the prebuilt engine this crate re-exports: the set
//! the Lumen release of this crate's version ships for the target, or the one
//! `LUMEN_ENGINE_ARTIFACTS` names. See `lumenui_build::link_engine`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    lumenui_build::link_engine(env!("CARGO_PKG_VERSION"));
}
