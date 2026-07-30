# Add a new component to the Lumen framework

This walks the conventions for landing a new primitive in
`lumen-primitives`. Components live in the main ECS world; render-side
behaviour goes through the extract -> render boundary in the render
world.

## Survey before touching code

- `lumenc framework_status` (via MCP) - surfaces open TODO.md items per
  section. Check whether the component is already half-shipped
  (`[~]`) or queued (`[ ]`); avoid duplicating.
- `lumen/primitives/src/lib.rs` - existing primitives (drag, hover,
  press, scroll, tabs, tooltip, dialog). Mirror the smallest one that
  matches your component's shape.

## File layout

- `lumen/primitives/src/<thing>.rs` - Component + its driving system.
- `lumen/primitives/src/lib.rs` - plugin registration and exports.
- `lumenc/src/spawn.rs` - wire the new tag/attribute to the component.
- `lumenc/src/parser_html.rs` - parse the markup form if it's new.
- `lumen/lsp/...` - update completions + diagnostics for the new tag.

## Constraints

- Bevy 0.18 hierarchy: use `ChildOf` / `Children`, not the legacy
  `Parent`. The MCP snapshot already follows this.
- Two-world split: components mutating render-only data go in
  `app.render_world`, not `app.world`. Cross-world data passes through
  the extract step.
- Snapshot coverage: add the new component to `lumen/mcp/src/plugin.rs`
  (`snap_*` sweeps) and `lumen/mcp/src/snapshot.rs` (`EntityInspect`
  field) so MCP introspection sees it from day one.
- Tests: ship a unit test under the relevant crate AND a golden-image
  pass in `lumen-render-headless/tests/` if the component renders.

## Definition of done

1. `cargo build` clean across the workspace.
2. `cargo clippy --no-deps --all-targets -- -D warnings` clean.
3. `cargo test -p lumen-primitives -p lumen-mcp -p lumenc` green.
4. `lumenc check apps/<some-app>` succeeds with the new tag used.
5. `lumenc snapshot --text` shows the new component on a sample app.
