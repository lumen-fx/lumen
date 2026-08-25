# Release checklist

How to cut a Lumen release. `.github/workflows/build-toolchain.yml` builds the
assets and `.github/workflows/release.yml` publishes them and starts every
channel that follows; a tag push is the only trigger for any of it, and no
target has a manual upload step. Asset names are load-bearing, because
`tools/release/install.sh` looks them up verbatim, so the workflow generates
them rather than anyone typing them.

| Asset                          | Built by                              |
| ------------------------------- | -------------------------------------- |
| `lumen-linux-x86_64.tar.gz`    | GitHub Actions, `ubuntu-latest`        |
| `lumen-linux-aarch64.tar.gz`   | GitHub Actions, `ubuntu-24.04-arm`     |
| `lumen-macos-aarch64.tar.gz`   | GitHub Actions, `macos-latest`         |
| `lumen-macos-x86_64.tar.gz`    | GitHub Actions, `macos-26-intel`       |
| `lumen-windows-x86_64.msi`     | GitHub Actions, `windows-latest`       |
| `lumen-windows-x86_64.zip`     | GitHub Actions, `windows-latest`       |
| `sha256sums.txt`               | GitHub Actions, the publish job        |

The `.msi` is the Windows install channel and the `.zip` is the portable
alternative, so the Windows leg publishes both. `install.sh` never fetches
either one: on Windows it prints the `.msi` URL and stops.

`sha256sums.txt` is `sha256sum` output covering every other asset in the
table, one `<hex>  <filename>` line each. The publish job generates it from
the artifacts it is about to upload, so a partial release (one build leg
failed) gets a checksum file listing exactly what it published.

Every leg builds natively for its own target; none of this is cross-compiled.
`linux-aarch64` and `macos-x86_64` have no equivalent leg in `ci.yml`, whose
matrix is `ubuntu-latest`, `macos-latest`, and `windows-latest` only, so a
regression specific to either one only shows up when a release is cut, not on
every pull request.

There is one component, `lumen`, meaning `lumenc`, the `liblumen` runtime
library (the `lumen` crate, built as a shared library), and `lumen-launcher`, the
stub `lumenc package` copies to make an app executable. Lumen's candela
scripting support is compiled into `liblumen` directly; there is no separate
candela binary and nothing here builds or ships one. The standalone candela
language toolchain is a different product with its own repository
(`lumen-fx/candela`) and its own release process, and is out of scope for this
checklist.

## One-time setup

- Actions enabled on the repository (already on).
- Publishing the release needs no secrets: the release job uses the built-in
  `GITHUB_TOKEN` (`contents: write`, scoped to that job only).
- Moving `main` to the next version afterwards needs one, `REPIN_DEPLOY_KEY`,
  holding the private half of the repository's write deploy key. The ruleset on
  `main` requires a pull request and lets deploy keys past it, so this is what a
  workflow pushes there with. `grammar-pins.yml` uses the same key for the same
  reason.
- Publishing to the package managers afterwards does need credentials, one per
  channel; see the section on them below.

## Cutting a release

1. Make sure `main` is green in the `ci` workflow.
2. Check that `version` in the workspace `Cargo.toml` is the version you are
   about to tag. It usually is already, because the previous release set it
   (step 7). If it is not, run `tools/release/bump-version.py <version>`,
   commit, push, and wait for green. The tag has to match this value: the
   release workflow compares them first and publishes nothing if they differ,
   because the MSI's version, the install receipt, and `lumenc --version` all
   read from these two places.
3. Tag and push:

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

4. The `release` workflow then, automatically, for each target:
   - checks out at the tag and builds `lumenc`, `liblumen`, and the launcher
     stub in release mode (`cargo build --release` with `-p lumenc`,
     `-p lumen`, and `-p lumen-launcher`; the workspace
     `[profile.release]` already strips symbols, so there is no separate
     strip step);
   - downloads the `lumenc new` templates with `tools/fetch-templates.sh`, one
     per template from the repository it is maintained in under `lumen-fx`;
   - packages `bin/lumenc` (`lumenc.exe` on Windows), the liblumen shared
     library, and `bin/lumen-launcher` into one archive, all in the *same*
     `bin/` directory, along with the two trees `lumenc` reads from beside
     itself: the candela standard library in `bin/libs` and the templates in
     `bin/templates`. See the note on `crates/lumenc/src/loader.rs` below;
   - on Windows, also stages an install receipt and builds
     `lumen-windows-x86_64.msi` from `tools/release/msi/lumen.wxs`. The
     receipt is staged after the zip is closed, so only the MSI carries one: a
     receipt
     marks a copy as installed, and an installed copy is the only kind that
     checks for updates;
   - once every target has finished (or failed), hashes every archive that
     did get built into `sha256sums.txt`, creates the release for the tag if
     it does not exist, and uploads the archives and the checksum file.

   Beside the per-target archives it builds the browser runtime once, as
   `lumen-web.tar.gz`. That pair is WebAssembly, so it is the same file on
   every platform; `lumenc web` downloads it the first time a site needs it,
   from the release it resolves through the releases page. The recipe is
   `.github/scripts/build-web-runtime.sh`, the same script `ci.yml` measures
   and runs a browser against.

   A single failed target does not block the others: the publish step runs
   even if a build leg failed, and only what succeeded gets uploaded.
   Re-running the workflow after a fix is safe, because `gh release upload
   --clobber` replaces same-named assets rather than erroring on them.

5. The same run then goes on to the four channels a release feeds, each in the
   workflow that owns it and each checked out at the tag: `publish.yml` for the
   language registries, `publish-packages.yml` for the OS package managers,
   `publish-extensions.yml` for the editor marketplaces, and `site-rebuild.yml`
   for the docs site. They appear as jobs of the release run rather than as
   runs of their own.

   `release.yml` calls them instead of each one waiting on the release,
   because the release is created with the built-in `GITHUB_TOKEN` and GitHub
   raises no workflow events for anything that token writes. A workflow
   triggered by `release: published` never starts here.

   Every one of them keeps a manual trigger in the Actions tab, so a leg that
   failed is re-run on its own rather than by cutting another release, and
   every one skips the parts it has no credential for. Each of those files
   lists the credentials it wants at the top.

6. Work through [Verify](#verify) against the published release.

7. Check that `main` moved on. The release's last job commits
   `chore: set the workspace version to X.Y.Z+1` straight to `main`, so the tag
   push is the whole release and there is nothing left to merge.

   From there `main` carries a version with no release behind it, which is the
   point: `main` builds identify themselves as the version they will become,
   and step 2 of the next release has nothing left to do.

   That is safe because nothing turns a version number into a download
   address. Every version-keyed lookup asks the releases page what exists:
   `lumenc` resolves the release its toolchain files come from through
   `releases/latest` (or through its install receipt), the update check
   compares against `releases/latest`, and `crates/lumenc/build.rs` confirms
   the tag it needs is published before fetching source from it. A number with
   no tag behind it resolves to nothing and says so.

   The commit is pushed over SSH with the write deploy key in
   `REPIN_DEPLOY_KEY`, and `ci` runs against it on `main` like any other
   commit. Pushing it with the workflow's own `GITHUB_TOKEN` instead would land
   it with no run behind it at all, and the ruleset on `main` takes no pusher
   but a deploy key.

   The bump is always the next patch, whatever kind of release the tag was.
   To go somewhere else, run `tools/release/bump-version.py 0.2.0` and land
   that. The script takes the version to set and moves every place the version
   is written out: the workspace package, each internal dependency pin,
   `sdk/rust-dylib` (outside the workspace, so it cannot inherit one),
   `Cargo.lock`, and the Python SDK. It writes nothing at all if one of those
   comes out unchanged, so a file that grew a version literal nobody told it
   about stops the bump instead of shipping a skew.

   If `main` is still on the version you tagged, look at the
   `move main to the next version` job in the release run. It reports what it
   decided and does nothing when the decision is not its to make:

   - The job never ran, because the release job did not finish. Fix what
     failed and re-run the workflow; the bump follows the release, and a
     release that published only some of its archives still reaches it.
   - `is not a plain vX.Y.Z tag`. Prereleases and other tag shapes are left
     alone. Run `tools/release/bump-version.py` yourself.
   - `main is at N, at or past ...`. The bump already landed, or this is a
     re-run of an older release. Nothing to do.
   - `a file that always moves did not`. A version literal changed shape, or
     one appeared somewhere new. The job names the file; teach
     `bump-version.py` about it and bump by hand this once.
   - `main is dirty the moment it is checked out`. Something rewrites a tracked
     file as the runner checks it out, and the bump commit would carry it. Find
     what, because the commit goes to `main` unreviewed.
   - `no REPIN_DEPLOY_KEY` or `could not be pushed to main`. The key is gone
     from the repository secrets, or it no longer bypasses the ruleset on
     `main`. The job fails rather than opening a pull request instead, so that
     a broken push is repaired now and not discovered by the next release
     failing its tag-versus-version check. Restore the key, then run
     `tools/release/bump-version.py` and land the bump by hand this once.

   Re-running a release is safe: the first run leaves `main` past the version
   the tag was cut for, and every later run of that tag, or of an older one,
   reads `main`, finds it already there, and stops without committing.

## Why liblumen goes in bin/, not lib/

`lumenc` does not link `lumen` at compile time; it `dlopen`s the shared
`liblumen` library at run time (see `crates/lumenc/src/loader.rs`). Its search
order is: next to its own executable, then an `LUMEN_LIB_DIR` override,
then the platform loader's default search path. It does not look in a
sibling `lib/` directory. A prebuilt install that put `lumenc` in `bin/`
and `liblumen.*` in `lib/` would install a `lumenc` that cannot find its
own runtime. `lumenc package` looks for the launcher stub the same way, so it
belongs in `bin/` too. The archive therefore puts all three files there:

| Platform | Files in `bin/`                                 |
| -------- | ----------------------------------------------- |
| Linux    | `lumenc`, `liblumen.so`, `lumen-launcher`       |
| macOS    | `lumenc`, `liblumen.dylib`, `lumen-launcher`    |
| Windows  | `lumenc.exe`, `lumen.dll`, `lumen-launcher.exe` |

## Publishing to the install channel

`curl -fsSL https://lumenfx.dev/install.sh | sh` is how people get the
toolchain. The installer lives here, at `tools/release/install.sh`; the site
repo (`lumen-fx/site`) fetches it fresh from this repo at build time and serves
it from the Lumen landing page. It reads the release itself, with no API
call: the latest tag comes from the redirect
`https://github.com/lumen-fx/lumen/releases/latest` sends, a `--version` pin
is turned into a tag directly, and everything after that is a plain download
from `releases/download/<tag>/<asset>`. It fetches `sha256sums.txt` first and
verifies each download against the line for it, installing nothing it cannot
verify. There is no manifest and no separate download host: publishing a
release means attaching the assets to the GitHub release for the tag, which
`release.yml` does automatically.

A release with no `sha256sums.txt` cannot be installed by the script at all.
That is what `--version` pointing at a release from before this file existed
runs into, and the error says so.

Asset naming is the contract `install.sh` relies on to find a build. It
computes `<target>` from `uname -s` and `uname -m` and looks for
`lumen-<target>.tar.gz` verbatim among the release assets; nothing else
about an asset is used to identify it.

`<target>` is `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, or
`macos-aarch64`. Windows installs from the `.msi`, which the script never
downloads; it prints the link and stops. The asset name carries no version:
the release tag is the version, and GitHub scopes assets to the release they
were uploaded to.

`lumen-web.tar.gz` is named the same way and is not a target. The installer
skips it, and `lumenc` fetches it by that exact name
(`crates/lumenc/src/package_cli.rs`) from the release
`crates/lumenc/src/release.rs` resolves, verifying it against the same
`sha256sums.txt`.

`lumenc` follows the same split when it offers an update. On Unix it re-runs
`install.sh`; on Windows it downloads
`releases/latest/download/lumen-windows-x86_64.msi` and hands it to `msiexec`
after the current command exits, because Windows will not replace a running
`lumenc.exe`. That URL is fixed, so nothing here has to publish a per-release
link.

## Publishing to the package registries

The toolchain install channel above is one way to get Lumen; the language
registries are the other. `.github/workflows/publish.yml` covers them.
`release.yml` calls it once the release is published, and it can also be
started by hand from the Actions tab, where it defaults to a dry run.

| Package   | Registry  | What it is                   |
| --------- | --------- | ---------------------------- |
| `lumenui` | PyPI      | the Python SDK               |
| `lumenui` | crates.io | the Rust SDK                 |
| `lumenc`  | crates.io | the CLI, for `cargo install` |

The Python distribution is pure Python and ships no binary, so one wheel
covers every platform; it finds `liblumen` from an installed toolchain or a
checkout at run time (`sdk/python/README.md` documents the search order).

That table is the whole list. Every other crate in the workspace is
`publish = false`, because the engine does not travel through crates.io:
`crates/lumenc/build.rs` fetches the tagged source and builds `liblumen` and
the launcher beside the installed binary, so a `cargo install lumenc` gets the
engine without any of its pieces being registry packages.

Neither crates.io package can go out yet. `lumenc` names eleven `lumen-*`
crates as dependencies and `lumenui` names the engine, and cargo will not
publish a crate whose dependency is not on the registry. Closing that means a
published `lumenc` that carries no engine dependencies; until then
`tools/release/publish-crates.py --plan` refuses to start and lists exactly
which crates are in the way, and `cargo publish -p lumenc --dry-run` names the
first of them.

The script computes publish order from `cargo metadata`, reports what state
each crate is in, and publishes them one at a time:

```sh
tools/release/publish-crates.py --plan      # order and per-crate state
tools/release/publish-crates.py --dry-run   # package and verify, upload nothing
tools/release/publish-crates.py --execute   # publish, needs CARGO_REGISTRY_TOKEN
```

It skips versions already on the registry, so a run interrupted halfway is
resumed by running it again. crates.io meters publishing (a burst of new
crates, then a slower drip), and the script waits out those intervals rather
than failing on them, which is why a first publish of the whole set takes
hours while a later release takes minutes.

Two things it refuses to start on, and both are worth knowing before a
release:

- A crate name on crates.io that belongs to another project. The script
  compares the `repository` field of an existing crate against this one.
- A dependency taken from git with no version. crates.io accepts no such
  dependency, so the crate carrying it, and everything above it, cannot be
  published. `lumen-script-candela` is in that state until candela publishes.

The setup the workflow needs (a `CRATES_IO_TOKEN` secret, a PyPI trusted
publisher, a `PYPI_PUBLISH_ENABLED` variable) is listed at the top of
`.github/workflows/publish.yml`. Each leg checks its own preconditions and
skips when one is missing, so running the workflow before the setup exists
reports what is missing instead of failing.

The PyPI leg needs one thing settled before `PYPI_PUBLISH_ENABLED` is turned
on. A trusted publisher is registered against a workflow filename, and PyPI
matches that name against the file the publishing job is defined in, which is
`publish.yml`; its documentation also lists a workflow started by another
workflow as unsupported. So register the publisher for `publish.yml`, turn the
variable on, and cut a release to see whether the upload is accepted. If PyPI
rejects the identity, move the upload step up into `release.yml` and change the
registered filename to match. Nothing has gone to PyPI yet, so there is no
working registration to lose either way.

## Publishing to the package managers

Lumen is also published to Homebrew, the AUR, scoop, and winget. The manifests
live in this directory, one per manager, and
`.github/workflows/publish-packages.yml`, which `release.yml` calls with the
tag it just published, takes them to the repository that serves each one: the
AUR over SSH, the tap (`lumen-fx/homebrew-lumen`) and the bucket
(`lumen-fx/scoop-lumen`) over a push, and winget as a pull request against
`microsoft/winget-pkgs` raised by `wingetcreate`.

The version and checksums in the manifests come from the release's
`sha256sums.txt`. `tools/release/update-package-manifests.sh <version>` does
that rewrite and checks the result; run it by hand to see what a release would
publish, or to repair a manifest that drifted.

Each publishing job checks its secret and its target repository first and stops
with a notice when either is missing, so the workflow is harmless before the
accounts exist. The workflow's header comment lists the secrets and the
repositories to create. Prereleases, drafts, and releases with no
`sha256sums.txt` are skipped.

Two of the four channels need something once, by hand:

- **winget.** Submit this directory's `tools/release/winget` manifests as
  `manifests/l/LumenFX/Lumen/<version>/` in a pull request against
  `microsoft/winget-pkgs`. `wingetcreate` updates an existing package but
  cannot create one, so every release after the first is automatic.
- **The tap and the bucket.** Create `lumen-fx/homebrew-lumen` and
  `lumen-fx/scoop-lumen`. They can be empty; the workflow writes
  `Formula/lumen.rb` and `bucket/lumen.json` into them.

The AUR needs no manual first step: the first push to
`ssh://aur@aur.archlinux.org/lumen-bin.git` creates the package.

Only the winget package installs the `.msi`. Homebrew, the AUR, and scoop all
install from the archives, which carry no receipt, so `lumenc` never offers to
update itself out from under a package manager that owns the version.

## Verify

- The release page (`https://github.com/lumen-fx/lumen/releases/tag/vX.Y.Z`)
  lists every asset from the table above that a build leg produced.
- `sha256sums.txt` is among them and covers every one of the others:

  ```sh
  base=https://github.com/lumen-fx/lumen/releases/download/vX.Y.Z
  curl -fsSLO "$base/sha256sums.txt"
  curl -fsSLO "$base/lumen-linux-x86_64.tar.gz"
  sha256sum --ignore-missing -c sha256sums.txt
  ```

  An asset with no line in that file cannot be verified, and `install.sh`
  refuses to install it.

- The installer runs end to end against the real release, into a throwaway
  prefix:

  ```sh
  curl -fsSL https://lumenfx.dev/install.sh |
    sh -s -- --prefix /tmp/lumen-check --no-confirm
  /tmp/lumen-check/bin/lumenc --version
  curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --prefix /tmp/lumen-check --uninstall --no-confirm
  ```

  A checksum mismatch here means the uploaded asset does not match the line
  for it in `sha256sums.txt`: re-run the release workflow for that target,
  which regenerates both.

- An already-installed `lumenc` finds out about this release from the tag
  alone, and so does `install.sh` when no `--version` is given: both follow
  `releases/latest` and read the last path segment of the redirect. That
  works while tags are `vX.Y.Z`; a tag in any other shape leaves every
  installed copy quiet about the release. Confirm the redirect points at the
  new tag:

  ```sh
  curl -fsSI https://github.com/lumen-fx/lumen/releases/latest |
    grep -i '^location'
  ```

- The `msi-smoke` workflow is green. It installs the package on a Windows
  runner, checks the file layout, the receipt contents (including that no
  install is pinned), and the user PATH entry, runs the installed `lumenc`,
  upgrades to a second version and confirms Windows records one product
  rather than two, then uninstalls and confirms nothing is left behind. It
  runs on every pull request that touches `tools/release/msi/`,
  `msi-smoke.yml`, or `build-toolchain.yml`, and it can be started by hand
  from the Actions tab.

## Windows checks that need a person

Two things about the MSI cannot be automated, so do them once per release on a
real desktop.

- **The SmartScreen path.** The package is unsigned, so a fresh download shows
  "Windows protected your PC". Walk it: "More info", then "Run anyway", and
  confirm the install completes. The docs describe this dialog, and the
  wording changes between Windows releases.
- **Updating while `lumenc` is running.** Start `lumenc run` on some app from a
  terminal, answer `y` to the update prompt, exit, and confirm the install
  finishes on its own and a new terminal picks up the new version. Windows
  will not replace a running executable, so this path defers the install until
  the process exits and is the one most likely to break silently.

The same two checks cover the winget package, which installs this `.msi`.

## Nightly builds

`.github/workflows/nightly.yml` runs every night and publishes a build of
`main`. It calls the same `build-toolchain.yml` a release does, so a green
nightly is evidence the next release will build, and a nightly that breaks on a
day nobody pushed anything points at a runner image, a system dependency, or an
upstream crate rather than at the workspace.

It publishes onto one rolling tag, `nightly`, force-moved to the commit it
built, with the assets clobbered onto the prerelease already sitting there.
Download links never change and only the newest build is reachable. A run where
one build leg failed publishes the rest and drops the leftover asset for the
target that failed, because `sha256sums.txt` covers that run alone and
`install.sh` refuses anything it cannot verify. A run where every leg failed
publishes nothing and leaves the tag where it was.

Two properties keep the nightly out of the release channel, and both are worth
checking if you edit either workflow:

- The tag is `nightly`, with no `v`. `release.yml` fires on `push: tags: v*`,
  and its last job walks the version on `main` forward, so a `v`-prefixed
  nightly tag would cut a release and bump the version every night.
- The release is a prerelease, and the flag is set on every run rather than
  only when the release is created. Every version-keyed lookup in the product
  resolves through the `releases/latest` redirect, which skips prereleases, so a
  nightly that ever lands as an ordinary release becomes the version
  `install.sh` installs and the update check offers.

The nightly publishes no Windows installer. The MSI's product GUIDs and version
are what Windows identifies an installation by, and a nightly stamped with a
version a release also carries would upgrade, replace, or block a real install
through those same GUIDs. Giving the nightly its own product identity means new
fixed GUIDs, a second entry in Installed apps, and two products appending to
`PATH`; that is a packaging decision, not something the nightly needs. The
portable zip carries no receipt, so a nightly unpacked from it never claims to
be installed and never checks for updates.

Start a nightly by hand from the Actions tab when you want one outside the
schedule.

None of the publishing workflows starts on its own. `release.yml` calls all
four, and the only thing that starts `release.yml` is a `v`-prefixed tag, so a
nightly reaches none of them: not crates.io or PyPI, not the package managers,
not the editor marketplaces, not the docs site. Running one by hand from the
Actions tab is the only way to point it at a nightly, and
`publish-packages.yml` refuses even that, because it reads the release it was
given and stops on a prerelease. Anything new that publishes on a release
belongs in that list of calls rather than on a trigger of its own.

## Current limitations

- `linux-aarch64` (`ubuntu-24.04-arm`) and `macos-x86_64`
  (`macos-26-intel`) are not covered by `ci.yml`'s own matrix, so a
  portability regression on either one is only caught when a release tag is
  pushed, not on every pull request.
- There is no GitHub-hosted Windows-on-Arm runner, so `windows-aarch64` is
  never built. `install.sh` handles that target being absent already (it
  prints the release page link instead of failing); nobody has asked for it
  yet.
- The MSI is unsigned. Every download trips SmartScreen until there is a
  code-signing certificate to sign it with. The community repository takes an
  unsigned package, so this does not hold winget up, but a winget install runs
  the same installer and shows the same dialog.
- Releases published before `sha256sums.txt` was part of the workflow have no
  checksum file, so `install.sh --version` cannot install them.
- `tools/release/release-assets.sh` targets a Gitea instance's API and is
  unrelated to this workflow; it is not part of the GitHub release path.
