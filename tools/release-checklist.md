# Release checklist

How to cut a lumenc release. CI (Gitea Actions, `.gitea/workflows/release.yml`)
builds and publishes the Linux binaries; macOS and Windows are built by hand
for now. Asset names are load-bearing — `tools/install.sh` downloads them by
exact name:

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
   is marked failed — fix, then rerun the workflow; re-uploads replace
   same-named assets, so reruns are safe.

## Manual binaries

Upload with `tools/release-assets.sh` (needs `curl` + `jq` and a personal
access token with `write:repository`), or through the release page in the web
UI.

macOS — on any Apple Silicon Mac with rustup:

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

Windows — on a Windows machine with rustup and the MSVC build tools:

```powershell
cargo build --release -p lumenc
copy target\release\lumenc.exe lumenc-windows-x86_64.exe
# upload via the release page, or release-assets.sh from Git Bash
```

## Verify

- The release page lists all intended assets with the exact names above.
- Spot-check the Linux binary:

  ```sh
  curl -fSLO https://git.example.com/lumen-fx/lumen/releases/download/vX.Y.Z/lumenc-linux-x86_64
  chmod +x lumenc-linux-x86_64 && ./lumenc-linux-x86_64 --version
  ```

- Note: `tools/install.sh` currently downloads from
  `https://github.com/lumen-ui/lumen/releases`. Until that mirror carries the
  release (or the installer is pointed at this host), `install.sh` will report
  "no release asset for this platform yet" — it fails cleanly, but end users
  won't get the new version through it. Mirroring the tag + assets to GitHub,
  or repointing `REPO` in the installer, closes that gap.

## Current limitations

- macOS cannot be cross-built from the Linux runner (Apple SDK and toolchain
  are Mac-only); automating it needs a Mac registered as a second runner.
- Windows could move to a cross build (`cargo-xwin`) later; kept manual until
  someone verifies the result on a real Windows box.
- Workflows on the Gitea host are read from `.gitea/workflows/` (first
  directory found wins, so these supersede `.github/workflows/` there).
  `.github/workflows/` still applies on GitHub mirrors.
