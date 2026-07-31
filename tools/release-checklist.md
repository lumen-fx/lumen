# Release checklist

How to cut a lumenc release. CI (Gitea Actions, `.gitea/workflows/release.yml`)
builds and publishes the Linux binaries; macOS and Windows are built by hand
for now. Asset names are load-bearing, so keep them exact:

| Asset                       | Built by                          |
| --------------------------- | --------------------------------- |
| `lumenc-linux-x86_64`       | CI, native                        |
| `lumenc-linux-aarch64`      | CI, Debian multiarch cross build  |
| `lumenc-macos-x86_64`       | manual, on a Mac                  |
| `lumenc-macos-aarch64`      | manual, on a Mac                  |
| `lumenc-windows-x86_64.exe` | manual, on a Windows machine      |

## One-time setup

- A runner registered on the Gitea instance with the `ubuntu-latest` label in
  Docker mode (the stock act_runner mapping to `catthehacker/ubuntu:act-latest`
  is fine for CI; the release job brings its own `rust:1-bookworm` container
  and only needs the label plus Docker Hub pull access).
- Cache enabled in the runner config (`[cache] enabled = true`) so CI builds
  are incremental. The release job deliberately builds cold.
- Actions enabled on the repository (already on).
- No extra secrets are required: the release job uses the built-in Actions
  token. To publish under a different identity, add a `RELEASE_TOKEN` secret
  (personal access token with `write:repository`); the workflow prefers it
  when present.

## Cutting a release

1. Make sure `main` is green in the `ci` workflow.
2. Bump `version` in the workspace `Cargo.toml`, commit, push, wait for green.
3. Tag and push:

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

4. The `release` workflow then, automatically:
   - builds `lumenc` in release mode for `x86_64-unknown-linux-gnu` (native)
     and `aarch64-unknown-linux-gnu` (cross, via `gcc-aarch64-linux-gnu` and
     `libgtk-3-dev:arm64` from Debian multiarch), stripping both;
   - creates the release for the tag if it does not exist;
   - attaches `lumenc-linux-x86_64` and `lumenc-linux-aarch64` through the
     Gitea API (`tools/release-assets.sh`).

   If the aarch64 step fails, the x86_64 asset is still uploaded and the run
   is marked failed - fix, then rerun the workflow; re-uploads replace
   same-named assets, so reruns are safe.

## Manual binaries

Upload with `tools/release-assets.sh` (needs `curl` + `jq` and a personal
access token with `write:repository`), or through the release page in the web
UI.

macOS - on any Apple Silicon Mac with rustup:

```sh
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --release -p lumenc --target aarch64-apple-darwin
cargo build --release -p lumenc --target x86_64-apple-darwin
cp target/aarch64-apple-darwin/release/lumenc lumenc-macos-aarch64
cp target/x86_64-apple-darwin/release/lumenc  lumenc-macos-x86_64
strip lumenc-macos-*

GITEA_TOKEN=<token> \
GITEA_SERVER=https://git.example.com \
GITEA_REPO=lumen-fx/lumen \
  tools/release-assets.sh vX.Y.Z lumenc-macos-aarch64 lumenc-macos-x86_64
```

Windows - on a Windows machine with rustup and the MSVC build tools:

```powershell
cargo build --release -p lumenc
copy target\release\lumenc.exe lumenc-windows-x86_64.exe
# upload via the release page, or release-assets.sh from Git Bash
```

## Publishing to the install channel

`curl -fsSL https://lumenfx.dev/install.sh | sh` is how people get the
toolchain. The installer is a static file on the site (the `lumen-fx/site`
repo, `apps/lumen/public/install.sh`); it reads a static manifest next to it
and pulls archives from `https://dl.lumenfx.dev`, an R2 bucket. There is no
server side to this: publishing a release means uploading objects and
committing one JSON file.

The manifest names two components. `lumen` is lumenc plus liblumen, installed
by default. `candela` is the standalone candela toolchain, opt-in with
`--components "add:candela"`. Each component ships one archive per target,
holding the tree to install: executables in `bin/`, libraries in `lib/`.

| Archive                         | Contents                            |
| ------------------------------- | ----------------------------------- |
| `lumen-X.Y.Z-<target>.tar.gz`   | `bin/lumenc`, `lib/liblumen.*`      |
| `candela-X.Y.Z-<target>.tar.gz` | `bin/candela`, `bin/candela-vm`     |

Targets are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`,
and `windows-x86_64`. Windows ships a `.zip`; the installer points Windows
users at it rather than unpacking it.

1. Pack one archive per component and target from the release binaries:

   ```sh
   mkdir -p stage/lumen/bin stage/lumen/lib
   cp target/release/lumenc      stage/lumen/bin/
   cp target/release/liblumen.so stage/lumen/lib/
   tar -czf dist/lumen-X.Y.Z-linux-x86_64.tar.gz -C stage/lumen .
   ```

2. Upload the archives to R2 under `<component>/<version>/<file>`, which is the
   path the manifest points at:

   ```sh
   for f in dist/*.tar.gz dist/*.zip; do
     name="$(basename "$f")"
     wrangler r2 object put "lumen-dl/${name%%-*}/X.Y.Z/$name" --file "$f"
   done
   ```

3. Build the manifest from the same files, so the checksums describe what was
   uploaded:

   ```sh
   tools/make-manifest.sh --version X.Y.Z --out manifest.json \
     lumen:linux-x86_64:dist/lumen-X.Y.Z-linux-x86_64.tar.gz \
     lumen:linux-aarch64:dist/lumen-X.Y.Z-linux-aarch64.tar.gz \
     lumen:macos-x86_64:dist/lumen-X.Y.Z-macos-x86_64.tar.gz \
     lumen:macos-aarch64:dist/lumen-X.Y.Z-macos-aarch64.tar.gz \
     lumen:windows-x86_64:dist/lumen-X.Y.Z-windows-x86_64.zip \
     candela:linux-x86_64:dist/candela-X.Y.Z-linux-x86_64.tar.gz
   ```

   Pass `--channel beta` or `--channel stable` when the release leaves alpha.
   Components missing a target are fine: the installer says which targets a
   component publishes and stops.

4. Publish it in the site repo, keeping a pinned copy so `install.sh --version
   X.Y.Z` keeps working after the next release:

   ```sh
   cp manifest.json site/apps/lumen/public/install/manifest.json
   cp manifest.json site/apps/lumen/public/install/manifest-X.Y.Z.json
   ```

   Commit both. Cloudflare Pages serves them at
   `https://lumenfx.dev/install/manifest.json` and
   `https://lumenfx.dev/install/manifest-X.Y.Z.json`.

## Verify

- The release page lists all intended assets with the exact names above.
- Spot-check the Linux binary:

  ```sh
  curl -fSLO https://git.example.com/lumen-fx/lumen/releases/download/vX.Y.Z/lumenc-linux-x86_64
  chmod +x lumenc-linux-x86_64 && ./lumenc-linux-x86_64 --version
  ```

- The manifest is live and describes the new release:

  ```sh
  curl -fsSL https://lumenfx.dev/install/manifest.json
  ```

- The installer runs end to end against it, into a throwaway prefix:

  ```sh
  curl -fsSL https://lumenfx.dev/install.sh |
    sh -s -- --prefix /tmp/lumen-check --no-confirm --components "add:candela"
  /tmp/lumen-check/bin/lumenc --version
  curl -fsSL https://lumenfx.dev/install.sh | sh -s -- --prefix /tmp/lumen-check --uninstall --no-confirm
  ```

  A checksum mismatch here means the bucket and the manifest disagree: rerun
  step 3 against the uploaded files.

## Current limitations

- macOS cannot be cross-built from the Linux runner (Apple SDK and toolchain
  are Mac-only); automating it needs a Mac registered as a second runner.
- Windows could move to a cross build (`cargo-xwin`) later; kept manual until
  someone verifies the result on a real Windows box.
- Workflows on the Gitea host are read from `.gitea/workflows/` (first
  directory found wins, so these supersede `.github/workflows/` there).
  `.github/workflows/` still applies on GitHub mirrors.
