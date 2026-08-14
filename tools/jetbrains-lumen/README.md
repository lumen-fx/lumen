# Lumen - JetBrains IDE plugin

Editor support for the [Lumen](https://lumenfx.dev) UI framework in
IntelliJ IDEA, CLion, PyCharm, WebStorm, and the other IntelliJ-based IDEs:
`.lmn` markup, Lumen CSS, and `.rhai` scripts.

Use it if you write Lumen apps in a JetBrains IDE. The VS Code extension in
`tools/vscode-lumen` covers the same languages for VS Code.

## Quick start

1. Build the language server:

   ```sh
   cargo build --release -p lumen-lsp
   ```

2. Build the plugin:

   ```sh
   ./gradlew buildPlugin
   ```

   The installable zip lands in `build/distributions/`.

3. In the IDE, install [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij)
   from the Marketplace, then install the zip with Settings | Plugins |
   gear icon | Install Plugin from Disk, and restart.

4. Put `lumen-lsp` on your `PATH`, or set its path in Settings |
   Languages & Frameworks | Lumen. With auto-discovery on, the plugin also
   finds a binary under `$CARGO_TARGET_DIR` or the project's `target/`
   directory, release before debug.

Open a `.lmn` file. The status of the server is in the LSP console
(View | Tool Windows | Language Servers).

## What you get

Syntax highlighting for `.lmn` (including embedded `<script>` and `<style>`
blocks) and `.rhai`, from the same TextMate grammars the VS Code extension
uses.

Everything else comes from `lumen-lsp`, so the editor and the compiler agree
on what counts as valid: diagnostics, completion, hover, signature help, go to
definition, find usages, rename, structure view, and reformatting a `.lmn`
file. See the
[tooling reference](https://docs.lumenfx.dev/reference/tooling/) for what the
server offers per file kind.

A `.css` file reaches the server only when it sits next to a `main.lmn`, that
is, inside a Lumen app directory, so stylesheets elsewhere keep the IDE's own
CSS support.

## Limitations

- LSP4IJ has to be installed first. It is the LSP client, and it is what makes
  the plugin work in Community-edition IDEs.
- The plugin does not ship `lumen-lsp`. Build it from this repository.
- There are no `lumenc` run/check/build actions and no live preview; the VS
  Code extension has those and this one does not yet.
- The plugin is not on the JetBrains Marketplace. Install the zip from disk.
- Enabling or disabling it takes an IDE restart, because it registers the
  TextMate bundle at startup.

## Development

Requires JDK 21. `./gradlew buildPlugin` produces the zip,
`./gradlew verifyPlugin` runs the JetBrains Plugin Verifier, and
`./gradlew runIde` starts a sandbox IDE with the plugin loaded.

The grammars are copied from `tools/vscode-lumen` during the build, so a
grammar fix reaches both editors.
