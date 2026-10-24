# Contributing

Lumen is in alpha. Issues and pull requests are welcome. APIs are not yet
stable, so open an issue to discuss larger changes before building them.

## Licensing

Lumen ships under the Mozilla Public License 2.0. To keep future licensing
options open, every pull request needs its authors to have signed the
[Contributor License Agreement](CLA.md). You keep the copyright in what you
write; the agreement grants the project the right to publish it under other
terms later.

The `cla` check on each pull request lists who still has to sign: the author
and committer of every commit, and every `Co-authored-by:` trailer that names a
GitHub account. Sign by commenting this line on the pull request, exactly as
written and on one line:

```
@lumenfx-bot sign I have read the CLA Document and I hereby sign the CLA
```

That is once per GitHub account, not once per pull request. The check turns
green on its own; comment `@lumenfx-bot recheck` if it does not. A commit
whose author email is linked to no GitHub account fails the check until you
add the email to your account or re-author the commit.

## Before you build

The toolchain is pinned in `rust-toolchain.toml`; rustup picks it up on its
own. On Linux, install the system libraries the workspace links against with
`.github/scripts/linux-deps.sh`. See
[docs/src/contributing/building-lumen.md](docs/src/contributing/building-lumen.md)
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

See [Gates](docs/src/contributing/building-lumen.md#gates) for what a
failure here means and how to fix it.

Tests that need a GPU or a display probe for one and skip themselves with a
printed reason, so the suite runs unmodified on a headless machine.

CodeQL scans every pull request. A new security alert of high or higher
severity blocks the merge; fix the finding or dismiss it with a reason on the
Security tab.

## Breaking changes

A change is breaking when an app, script, or embedder that works on the last
release has to change something to work on the next one. That covers a
removed or renamed tag, attribute, CSS property, `lumen.toml` key, `lumenc`
flag, or script builtin; a changed default a user can see; a C ABI or SDK
change that needs a recompile or a code edit; and a compiled-artifact format
bump that needs a rebuild.

A breaking pull request carries the `C-Breaking-Change` label and adds a
migration note in the same change, as its own file under
`docs/migration/unreleased/`, for example
`docs/migration/unreleased/window-title-key.md`:

```markdown
# The `title` key moved from `[app]` to `[window]`

Who this affects, what changed, and what to do, with the code before and
after.
```

The first line is a `# ` heading that states the break in one line; it is
what the release notes list. The body says who is affected and how to
migrate. Notes are ASCII only. A check on every pull request fails when the
label and the note disagree, or when a note is not in this shape.

When a release is cut, the notes in its tag move to `docs/migration/vX.Y.Z/`,
the docs site renders them as that version's migration guide, and the release
notes link to it.

## Review

Every pull request is read before it merges. A change under `.github/`, the
Cargo manifests and lockfile, the toolchain pin, `tools/release/`, or
`tools/setup-lumen/` also pulls in a review request automatically
(`.github/CODEOWNERS`), because those paths decide what the build pulls in and
what a release publishes. Expect a slower read on them, and say in the pull
request why the change is needed rather than only what it does.

## After you open a pull request

lumenfx-bot reads every pull request as it changes and keeps two things
current without a maintainer at the keyboard:

- The `S-` label, which is the pull request's state: `S-Needs-Triage` when
  it opens, `S-Waiting-On-Author` while something is yours to do (an unsigned
  CLA, a red required check, a conflict, requested changes), and
  `S-Needs-Review` once nothing is. Area (`A-`) and class (`C-`) labels are
  set from the touched paths and the title prefix when none are present.
- One status comment, posted when you have something to do and edited from
  then on. It lists what is left: the CLA, each required check with a link,
  a docs reminder when code changed and no doc did.

Talk to the bot with a comment that starts with `@lumenfx-bot`, for example
`@lumenfx-bot ready` once you have addressed requested changes. The full
command list is at https://hook.lumenfx.dev/help.

On a first pull request from a new account, GitHub holds the checks until a
maintainer starts them.

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
order, and the one-line description of each (`public/lumenc/src/cli/scaffold.rs`).
Adding a template means a new repository upstream and an entry here.

Run `tools/fetch-templates.sh` to download the templates for a local test run.
Cases that scaffold an app skip themselves with a printed reason without them.

## Style

- `cargo fmt` is law.
- `cargo clippy --workspace --all-targets -- -D warnings` is law.
- Public items get a one-line `///`.
- A change a user can observe updates its documentation page under `docs/` in
  the same pull request.
