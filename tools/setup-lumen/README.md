# setup-lumen

A GitHub Action that installs the Lumen toolchain on a runner and puts `lumenc`
on `PATH`.

Use it in the workflows of an app built with Lumen, where the job needs
`lumenc` to check, build, or package the app. It downloads a published release
rather than building the framework, so a job that only uses the toolchain needs
no Rust toolchain of its own.

## Quick start

```yaml
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: lumen-fx/lumen/tools/setup-lumen@main
      - run: lumenc check .
```

Pin the action to a release tag instead of `main` to keep a workflow on a known
version of the action itself.

## Inputs

| Input | Default | Effect |
| --- | --- | --- |
| `version` | `latest` | Release to install. A version number such as `0.0.3`, a tag such as `v0.0.3`, or `latest`. |
| `cache` | `true` | Keep the unpacked toolchain in the workflow cache, keyed on the release and the runner's target, so later runs skip the download. |

## Outputs

| Output | Value |
| --- | --- |
| `version` | The release that was installed, without the leading `v`. |
| `tag` | The release tag that was installed. |
| `target` | The target the toolchain was built for, such as `linux-x86_64`. |
| `bin-path` | The directory holding `lumenc`, added to `PATH`. |
| `cache-hit` | Whether the toolchain came back from the cache. |

## What it installs

The release archive for the runner's platform: `lumenc`, the `liblumen` runtime
library it loads, and the launcher stub `lumenc package` builds an app
executable from. Every download is checked against the `sha256sums.txt`
published with the release before anything is unpacked, and nothing is
installed if the two disagree.

Windows runners get the portable zip, not the MSI. The MSI writes an install
receipt, and a receipt is what turns `lumenc`'s update check on; a runner has
no use for either, and the check stays off in CI regardless.

## Limitations

- Windows releases are x86_64 only. A Windows Arm runner has no archive to
  install and the action stops with that message.
- `lumenc run` loads the runtime library, which links GTK, ALSA, X11, and
  Wayland on Linux. Install those in the job before a headless run; see
  [Build Lumen from source](../../docs/docs/contributing/building-lumen.md) for
  the list. `lumenc check`, `new`, and `fmt` need none of them.
- Releases published before `sha256sums.txt` existed cannot be installed. There
  is nothing to verify them against, and the action stops rather than skipping
  the check.

## More

- [Install Lumen](../../docs/docs/getting-started/install.md), including the
  installer for a workstation and the continuous-integration notes.
- [Every `lumenc` subcommand and flag](../../docs/docs/reference/cli.md).
