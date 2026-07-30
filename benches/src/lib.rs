//! Benchmark harness crate.
//!
//! - Individual benches live in `benches/benches/*.rs`, registered as `[[bench]]` entries in Cargo.toml.
//! - `cargo bench -p lumen-benches` runs the whole suite.
//! - Each bench uses criterion's `criterion_group!` + `criterion_main!` macros and writes baselines to `target/criterion/<group>/<bench>/`.
