# Contributing

Lumen is in alpha. Issues and pull requests are welcome. APIs are not yet
stable, so open an issue to discuss larger changes before building them.

## Licensing

Lumen ships under the Mozilla Public License 2.0. To keep future licensing
options open, every pull request needs its authors to have signed the
[Contributor License Agreement](CLA.md). You keep the copyright in what you
write; the agreement grants the project the right to publish it under other
terms later.

A bot checks this on each pull request and comments if a signature is missing.
Sign by replying to the pull request with a comment containing exactly:

```
I have read the CLA Document and I hereby sign the CLA
```

That is once per GitHub account, not once per pull request. The check turns
green on its own; comment `recheck` if it does not.

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

A fifth gate, also Linux-only, checks that the engine dylib
(`public/lumen-dylib`) still resolves the same crate graph the release ships:

```sh
python3 tools/verify-engine-crate-graph.py
```

See [Gates](docs/docs/contributing/building-lumen.md#gates) for what a
failure here means and how to fix it.

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

## Templates

The apps `lumenc new` scaffolds are not in this repository. Each is maintained
in a repository of its own under the
[lumen-fx](https://github.com/lumen-fx) organisation, named after the template,
and a Lumen release ships a copy of every one beside the toolchain. So a fix to
a template's markup, CSS, script, or README goes to that repository, and
reaches users with the next release.

What lives here is the gallery: which templates `lumenc new` offers, in which
order, and the one-line description of each (`public/lumenc/src/scaffold.rs`).
Adding a template means a new repository upstream and an entry here.

Run `tools/fetch-templates.sh` to download the templates for a local test run.
Cases that scaffold an app skip themselves with a printed reason without them.

## Style

- `cargo fmt` is law.
- `cargo clippy --workspace --all-targets -- -D warnings` is law.
- Public items get a one-line `///`.
- A change a user can observe updates its documentation page under `docs/` in
  the same pull request.
