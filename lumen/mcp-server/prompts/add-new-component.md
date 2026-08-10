# Add a new component to the Lumen framework

This walks the conventions for landing a new primitive in
`lumen-primitives`. Components live in the main ECS world; render-side
behaviour goes through the extract -> render boundary in the render
world.

## Survey before touching code

- `lumen/primitives/src/lib.rs` - the existing primitives (baseline, checkbox,
  controls, cursor, drag, hover, popup, press, progress, radio, scroll,
  scrollbar, state_style, switch, tabs, tooltip, transition, validation, wake).
  Mirror the smallest one that matches your component's shape.
- `lumen_snapshot_text` on a running app - shows what the introspection layer
  already reports, which is the shape your component has to fit into.

## File layout

- `lumen/primitives/src/<thing>.rs` - Component plus its driving system.
- `lumen/primitives/src/lib.rs` - plugin registration and exports.
- `lumen/runtime/src/spawn.rs` - wire the new tag or attribute to the
  component.
- `lumenc/src/parser_html.rs` - parse the markup form if it is new.
- `lumen/lsp/src/docs.rs` - add the tag or attribute to `TAGS` / `ATTRS`, its
  hover documentation, and any fixed value set.

## Constraints

- Hierarchy: use `ChildOf` and `Children`. `Parent` no longer exists. The MCP
  snapshot already follows this.
- Two-world split: components mutating render-only data go in
  `app.render_world`, not `app.world`. Cross-world data passes through
  the extract step.
- Snapshot coverage: add the new component to `lumen/mcp/src/plugin.rs`
  (the `snap_*` sweeps) and `lumen/mcp/src/snapshot.rs` (an `EntityInspect`
  field) so MCP introspection sees it from day one.
- Docs: document the new tag or attribute on the reference page that owns it,
  in the same change.
- Tests: ship a unit test under the relevant crate, and a golden-image case in
  `lumen/render-headless/tests/` if the component renders.

## Definition of done

1. `cargo build --workspace` clean.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `cargo test -p lumen-primitives -p lumen-mcp -p lumenc` green.
4. `lumenc check apps/<some-app>` succeeds with the new tag used.
5. `lumenc snapshot --text` shows the new component on a sample app.
