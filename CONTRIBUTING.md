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

`cargo lint` is an alias for that clippy line, so the local run and the gate
are the same command.

The first workspace build points `core.hooksPath` at `.githooks`, and the
pre-commit hook there runs `fmt` and `clippy` on any commit that touches Rust.
A commit that touches none of it pays nothing. `git commit --no-verify` skips
the hook once, `LUMEN_SKIP_HOOKS=1` skips it for a session, and
`LUMEN_NO_HOOK_SETUP=1` stops the registration from happening at all. If you
already point `core.hooksPath` somewhere of your own, that is left alone.

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

## Review

Every pull request is read before it merges. A change under `.github/`, the
Cargo manifests and lockfile, the toolchain pin, `tools/release/`, or
`tools/setup-lumen/` also pulls in a review request automatically
(`.github/CODEOWNERS`), because those paths decide what the build pulls in and
what a release publishes. Expect a slower read on them, and say in the pull
request why the change is needed rather than only what it does.

## After you open a pull request

A workflow reads every pull request as it changes and keeps two things
current without a maintainer at the keyboard:

- A `triage` check on the Checks tab. Its summary lists the CLA state, each
  required check with a link, the labels, and the one next step: sign the
  CLA, fix a red leg, add the docs page, or wait for review. It never blocks
  a merge; it is there so you can see what is missing before anyone reads
  the diff.
- The `S-` label, which is the pull request's state: `S-Needs-Triage` when
  it opens, `S-Waiting-On-Author` while a required check is red, and
  `S-Needs-Review` once every check is green, the CLA is signed, and the
  pull request is not a draft. Area (`A-`) and class (`C-`) labels are set
  from the touched paths and the title prefix when none are present.

On a first pull request from a new account, GitHub holds the checks until a
maintainer starts them; the summary says so. Nothing in this workflow reads
or runs the code in the pull request.

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
