# Contributing

Lumen is in alpha. Issues and pull requests are welcome. APIs are not yet
stable, so open an issue to discuss larger changes before building them.

Licensing: to keep future licensing options open, substantial contributions
may be asked to sign a CLA before merge.

## Invariants you must not break

1. `lumen-core` may not import any impl crate.
2. Every backend trait must have at least one default impl and one alternative path (headless or stub) so removing the default does not break compile.
3. FFI surfaces use `#[repr(C)]`, never `#[repr(C, packed)]`.
4. No Rust panics may escape across the C-ABI boundary.

## Style

- `cargo fmt` is law.
- `cargo clippy --workspace --all-targets -- -D warnings` is law.
- Public items get a one-line `///`.
