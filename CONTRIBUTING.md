# Contributing

Lumen is in alpha. Issues and pull requests are welcome. APIs are not yet
stable, so open an issue to discuss larger changes before building them.

Licensing: to keep future licensing options open, substantial contributions
may be asked to sign a CLA before merge.

## Before you build

The toolchain is pinned in `rust-toolchain.toml`; rustup picks it up on its
own. On Linux, install the system libraries the workspace links against with
`.github/scripts/linux-deps.sh`. See
[docs/docs/contributing/building-lumen.md](docs/docs/contributing/building-lumen.md)
for the full setup.

## Gates

CI runs these on every pull request, and a red leg blocks the merge. Run them
locally first:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

`fmt` and `clippy` run on Linux only. The build and test jobs run on Linux,
macOS, and Windows; that matrix is also the release parity check, so a failure
on one OS is a portability gap to fix rather than an OS to drop.

Tests that need a GPU or a display probe for one and skip themselves with a
printed reason, so the suite runs unmodified on a headless machine.

CodeQL scans every pull request. A new security alert of high or higher
severity blocks the merge; fix the finding or dismiss it with a reason on the
Security tab.

## Invariants you must not break

1. `lumen-core` may not import any impl crate.
2. Every backend trait must have at least one default impl and one alternative
   path (headless or stub) so removing the default does not break compile.
3. FFI surfaces use `#[repr(C)]`, never `#[repr(C, packed)]`.
4. No Rust panics may escape across the C-ABI boundary.

## Style

- `cargo fmt` is law.
- `cargo clippy --workspace --all-targets -- -D warnings` is law.
- Public items get a one-line `///`.
- A change a user can observe updates its documentation page under `docs/` in
  the same pull request.
